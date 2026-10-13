#![cfg_attr(windows, windows_subsystem = "windows")]
use anyhow::{bail, Context, Result};
use clap::Parser;
use std::{path::PathBuf, time::Duration};
use tokio::sync::mpsc::UnboundedReceiver;
use vime::{
    config::Config,
    platform,
    protocol::{Connection, ServerEvent},
    session::{FinalCollector, ReceivedText},
    ControlEvent,
};

#[derive(Parser)]
#[command(
    version,
    about = "Minimal self-hosted Windows push-to-talk transcription"
)]
struct Args {
    #[arg(long)]
    config: Option<PathBuf>,
    #[arg(long)]
    check_config: bool,
    #[arg(long)]
    test_connection: bool,
    #[arg(long)]
    wav: Option<PathBuf>,
    #[arg(long)]
    no_clipboard: bool,
    #[arg(long)]
    device: Option<String>,
    #[arg(long)]
    log_file: Option<PathBuf>,
    #[arg(long)]
    foreground: bool,
    #[arg(long)]
    stop: bool,
}

enum Outcome {
    Final(String),
    Recovered { text: String, error: String },
    Cancelled,
    Shutdown,
}

static CONSOLE_CONTROL: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

async fn console_interrupt() {
    if *CONSOLE_CONTROL.get().unwrap_or(&false) {
        if tokio::signal::ctrl_c().await.is_ok() {
            return;
        }
        tracing::warn!("console interrupt unavailable; use --stop to shut down");
    }
    std::future::pending::<()>().await;
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let diagnostic = std::env::args().skip(1).any(|arg| {
        matches!(
            arg.as_str(),
            "--foreground"
                | "--check-config"
                | "--test-connection"
                | "--wav"
                | "--help"
                | "-h"
                | "--version"
                | "-V"
                | "--stop"
        ) || arg.starts_with("--wav=")
    });
    if diagnostic {
        platform::attach_parent_console();
    }
    if let Err(error) = run().await {
        tracing::error!(error = %error, "operation failed");
        eprintln!("vime: {error:#}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let args = Args::parse();
    let _ = CONSOLE_CONTROL.set(args.foreground || args.wav.is_some());
    if args.stop {
        return platform::request_shutdown();
    }
    let config_path = args
        .config
        .clone()
        .unwrap_or_else(|| PathBuf::from("vime.toml"));
    let log_path = args
        .log_file
        .clone()
        .unwrap_or_else(|| config_path.with_file_name("vime.log"));
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&log_path)
        .context("cannot open log file")?;
    tracing_subscriber::fmt()
        .with_ansi(false)
        .with_writer(std::sync::Mutex::new(file))
        .with_env_filter(tracing_subscriber::EnvFilter::new("info"))
        .init();
    let mut config = if args.config.is_some() || config_path.exists() {
        Config::load(&config_path)?
    } else {
        Config::default()
    };
    if args.no_clipboard {
        config.output.clipboard = false;
    }
    config.validate()?;
    if args.check_config {
        println!("configuration is valid");
        return Ok(());
    }
    if args.test_connection {
        let mut connection = Connection::connect(&config.server).await?;
        connection.cancel().await?;
        println!("connection and transcription session are ready");
        return Ok(());
    }
    if let Some(path) = args.wav {
        let samples = vime::audio::load_wav(path)?;
        if samples.is_empty() {
            bail!("WAV input is empty");
        }
        let (_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let mut received = ReceivedText::default();
        let result = transcribe_wav(&samples, &config, &mut controls, &mut received).await;
        let text = match recover_result(result, &received)? {
            Outcome::Final(text) => text,
            Outcome::Recovered { text, error } => {
                println!("{text}");
                vime::output::deliver_recovered(&text, &config.output).await?;
                bail!("{error}; received text was recovered and may be incomplete");
            }
            Outcome::Cancelled => bail!("recording was cancelled"),
            Outcome::Shutdown => return Ok(()),
        };
        // All diagnostics are separate from transcript stdout in WAV mode.
        println!("{text}");
        if !text.trim().is_empty() {
            vime::output::deliver(&text, &config.output).await?;
        }
        return Ok(());
    }
    let _instance = platform::SingleInstance::acquire()?;
    let mut hotkey = platform::hotkey::start(&config.hotkey)?;
    tracing::info!("ready; configured hotkey starts recording");
    if args.foreground {
        eprintln!("vime is ready; Ctrl+C stops the process");
    }
    loop {
        tokio::select! {
            _ = console_interrupt() => break,
            event = hotkey.events.recv() => match event {
                Some(ControlEvent::Start) => {
                    match record_live(&config, args.device.as_deref(), &mut hotkey.events).await {
                        Ok(Outcome::Final(text)) => {
                            if let Some(action) = pending_cancel(&mut hotkey.events) {
                                if matches!(action, Outcome::Shutdown) { break; }
                                hotkey.reset()?; continue;
                            }
                            if text.trim().is_empty() { tracing::info!("no speech returned"); }
                            else if let Err(error) = vime::output::deliver(&text, &config.output).await {
                                tracing::error!(error = %error, "final delivery failed; not retried");
                            } else { tracing::info!("final text delivered"); }
                        },
                        Ok(Outcome::Recovered { text, error }) => {
                            if let Some(action) = pending_cancel(&mut hotkey.events) {
                                if matches!(action, Outcome::Shutdown) { break; }
                                hotkey.reset()?; continue;
                            }
                            tracing::warn!(error = %error, "recovering received text; result may be incomplete");
                            if let Err(error) = vime::output::deliver_recovered(&text, &config.output).await {
                                tracing::error!(error = %error, "recovery delivery failed; not retried");
                            }
                        },
                        Ok(Outcome::Cancelled) => tracing::info!("recording cancelled"),
                        Ok(Outcome::Shutdown) => break,
                        Err(error) => tracing::error!(error = %error, "recording failed; ready for next recording"),
                    }
                    hotkey.reset()?;
                    // Key presses during finalizing/delivery are not queued recordings.
                    while let Ok(event) = hotkey.events.try_recv() {
                        if event == ControlEvent::Shutdown { return Ok(()); }
                    }
                },
                Some(ControlEvent::Shutdown) | None => break,
                _ => {},
            }
        }
    }
    tracing::info!("stopped");
    Ok(())
}

fn check_recording_event(event: ServerEvent, received: &mut ReceivedText) -> Result<()> {
    received.observe(&event)?;
    match event {
        ServerEvent::Error { message } => bail!("{message}"),
        ServerEvent::Final { .. } | ServerEvent::Committed { .. } => {
            bail!("ASR completed before the client committed audio")
        }
        _ => Ok(()),
    }
}

async fn record_live(
    config: &Config,
    device: Option<&str>,
    controls: &mut UnboundedReceiver<ControlEvent>,
) -> Result<Outcome> {
    let mut received = ReceivedText::default();
    let result = record_live_inner(config, device, controls, &mut received).await;
    recover_result(result, &received)
}

fn recover_result(result: Result<Outcome>, received: &ReceivedText) -> Result<Outcome> {
    match result {
        Err(error) => match received.recover() {
            Some(text) => Ok(Outcome::Recovered {
                text,
                error: error.to_string(),
            }),
            None => Err(error),
        },
        result => result,
    }
}

/// Cancellation takes precedence over a result already ready on the socket.
fn pending_cancel(controls: &mut UnboundedReceiver<ControlEvent>) -> Option<Outcome> {
    let mut cancelled = false;
    while let Ok(event) = controls.try_recv() {
        match event {
            ControlEvent::Cancel => cancelled = true,
            ControlEvent::Shutdown => return Some(Outcome::Shutdown),
            _ => {}
        }
    }
    cancelled.then_some(Outcome::Cancelled)
}

async fn transcribe_wav(
    samples: &[i16],
    config: &Config,
    controls: &mut UnboundedReceiver<ControlEvent>,
    received: &mut ReceivedText,
) -> Result<Outcome> {
    let mut connection = Connection::connect(&config.server).await?;
    for frame in samples.chunks(480) {
        connection.append(frame).await?;
        // Keep receiving deltas until the next frame is due, rather than speeding
        // up the entire file whenever a partial event arrives.
        let tick = tokio::time::sleep(Duration::from_millis(20));
        tokio::pin!(tick);
        loop {
            tokio::select! {
                biased;
                _ = console_interrupt() => return Ok(Outcome::Shutdown),
                _ = &mut tick => break,
                event = connection.next_event() => check_recording_event(event?, received)?,
            }
        }
    }
    connection.commit().await?;
    wait_final(
        &mut connection,
        controls,
        config.server.final_timeout_ms,
        received,
    )
    .await
}

async fn record_live_inner(
    config: &Config,
    device: Option<&str>,
    controls: &mut UnboundedReceiver<ControlEvent>,
    received: &mut ReceivedText,
) -> Result<Outcome> {
    let mut capture = platform::capture::start(device)?;
    // Capture starts before connecting, so initial syllables are buffered.
    // A full capture queue fails explicitly rather than delivering truncated audio.
    let connect = Connection::connect(&config.server);
    tokio::pin!(connect);
    let mut released = false;
    let mut connection = loop {
        tokio::select! {
            biased;
            event = controls.recv() => match event {
                Some(ControlEvent::Stop) => {
                    // Finish a short utterance even if the key was released during handshake.
                    capture.stop()?;
                    released = true;
                },
                Some(ControlEvent::Cancel) => return Ok(Outcome::Cancelled),
                Some(ControlEvent::Shutdown) | None => return Ok(Outcome::Shutdown),
                _ => {},
            },
            _ = console_interrupt() => return Ok(Outcome::Shutdown),
            result = &mut connect => break result?,
        }
    };
    tracing::info!("recording");
    let mut sample_count = 0usize;
    loop {
        if released {
            break;
        }
        tokio::select! {
            biased;
            _ = console_interrupt() => return Ok(Outcome::Shutdown),
            event = controls.recv() => match event {
                Some(ControlEvent::Stop) => { capture.stop()?; break; },
                Some(ControlEvent::Cancel) => return Ok(Outcome::Cancelled),
                Some(ControlEvent::Shutdown) | None => return Ok(Outcome::Shutdown),
                _ => {},
            },
            frame = capture.frames.recv() => match frame {
                Some(Ok(samples)) => {
                    sample_count += samples.len();
                    if let Some(outcome) = append_audio(&mut connection, &samples, controls, &mut released, &mut capture).await? { return Ok(outcome); }
                },
                Some(Err(error)) => bail!("audio capture failed: {error}"),
                None => break,
            },
            event = connection.next_event() => check_recording_event(event?, received)?,
        }
    }
    capture.stop()?;
    while let Some(frame) = capture.frames.recv().await {
        let frame = frame.map_err(|error| anyhow::anyhow!("audio capture failed: {error}"))?;
        sample_count += frame.len();
        if let Some(outcome) = append_audio(
            &mut connection,
            &frame,
            controls,
            &mut released,
            &mut capture,
        )
        .await?
        {
            return Ok(outcome);
        }
    }
    if sample_count == 0 {
        let _ = connection.cancel().await;
        return Ok(Outcome::Cancelled);
    }
    {
        let commit = connection.commit();
        tokio::pin!(commit);
        loop {
            tokio::select! {
                biased;
                event = controls.recv() => match event {
                    Some(ControlEvent::Cancel) => return Ok(Outcome::Cancelled),
                    Some(ControlEvent::Shutdown) | None => return Ok(Outcome::Shutdown),
                    _ => {},
                },
                _ = console_interrupt() => return Ok(Outcome::Shutdown),
                result = &mut commit => { result?; break; },
            }
        }
    }
    tracing::info!("waiting for final text");
    wait_final(
        &mut connection,
        controls,
        config.server.final_timeout_ms,
        received,
    )
    .await
}

async fn append_audio(
    connection: &mut Connection,
    samples: &[i16],
    controls: &mut UnboundedReceiver<ControlEvent>,
    released: &mut bool,
    capture: &mut platform::capture::Capture,
) -> Result<Option<Outcome>> {
    for frame in samples.chunks(480) {
        let append = connection.append(frame);
        tokio::pin!(append);
        loop {
            tokio::select! {
                biased;
                event = controls.recv() => match event {
                    Some(ControlEvent::Cancel) => return Ok(Some(Outcome::Cancelled)),
                    Some(ControlEvent::Shutdown) | None => return Ok(Some(Outcome::Shutdown)),
                    Some(ControlEvent::Stop) => { capture.stop()?; *released = true; },
                    _ => {},
                },
                _ = console_interrupt() => return Ok(Some(Outcome::Shutdown)),
                result = &mut append => { result?; break; },
            }
        }
    }
    Ok(None)
}

async fn wait_final(
    connection: &mut Connection,
    controls: &mut UnboundedReceiver<ControlEvent>,
    timeout_ms: u64,
    received: &mut ReceivedText,
) -> Result<Outcome> {
    let deadline = tokio::time::sleep(Duration::from_millis(timeout_ms));
    tokio::pin!(deadline);
    let mut collector = FinalCollector::default();
    loop {
        tokio::select! {
            biased;
            event = controls.recv() => match event {
                Some(ControlEvent::Cancel) => return Ok(Outcome::Cancelled),
                Some(ControlEvent::Shutdown) | None => return Ok(Outcome::Shutdown),
                _ => {},
            },
            _ = console_interrupt() => return Ok(Outcome::Shutdown),
            _ = &mut deadline => bail!("final transcription timed out"),
            event = connection.next_event() => {
                let event = event?;
                received.observe(&event)?;
                if let Some(text) = collector.observe(event)? {
                    // Dropping this isolated connection cannot leak late events into the next recording.
                    return Ok(Outcome::Final(text));
                }
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vime::mock::{MockOptions, Scenario};

    async fn connect_mock(scenario: Scenario) -> (Connection, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let config = vime::config::ServerConfig {
            url: format!("ws://{}", listener.local_addr().unwrap()),
            ..Default::default()
        };
        let task = tokio::spawn(async move {
            let _ = vime::mock::serve(
                listener,
                MockOptions {
                    scenario,
                    ..Default::default()
                },
            )
            .await;
        });
        (Connection::connect(&config).await.unwrap(), task)
    }

    #[tokio::test]
    async fn queued_cancel_and_shutdown_precede_ready_final() {
        for (event, shutdown) in [
            (ControlEvent::Cancel, false),
            (ControlEvent::Shutdown, true),
        ] {
            let (mut connection, task) = connect_mock(Scenario::Success).await;
            connection.append(&[100; 480]).await.unwrap();
            connection.commit().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
            tx.send(event).unwrap();
            let mut received = ReceivedText::default();
            let outcome = wait_final(&mut connection, &mut controls, 500, &mut received)
                .await
                .unwrap();
            if shutdown {
                assert!(matches!(outcome, Outcome::Shutdown));
            } else {
                assert!(matches!(outcome, Outcome::Cancelled));
            }
            assert!(received.recover().is_none());
            task.abort();
        }
    }

    #[tokio::test]
    async fn timeout_recovers_partial_but_cancellation_does_not() {
        let (mut connection, task) = connect_mock(Scenario::Timeout).await;
        connection.append(&[100; 480]).await.unwrap();
        let mut received = ReceivedText::default();
        check_recording_event(connection.next_event().await.unwrap(), &mut received).unwrap();
        connection.commit().await.unwrap();
        let (_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let result = wait_final(&mut connection, &mut controls, 20, &mut received).await;
        match recover_result(result, &received).unwrap() {
            Outcome::Recovered { text, .. } => assert_eq!(text, "你好，"),
            _ => panic!("expected recovered text"),
        }
        assert!(matches!(
            recover_result(Ok(Outcome::Cancelled), &received).unwrap(),
            Outcome::Cancelled
        ));
        task.abort();
    }
}
