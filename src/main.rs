#![cfg_attr(windows, windows_subsystem = "windows")]
use anyhow::{bail, Context, Result};
use clap::Parser;
use std::{path::PathBuf, time::Duration};
use tokio::sync::{mpsc::UnboundedReceiver, watch};
use vime::overlay::{DesktopCommand, UiPhase, UiSnapshot};
use vime::tray::TrayCommand;
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
    /// Keep the tray and hotkey active without showing subtitles.
    #[arg(long)]
    no_window: bool,
    #[arg(long)]
    no_tray: bool,
    /// Play a ten-second overlay preview without a microphone or server.
    #[arg(long, conflicts_with_all = ["wav", "test_connection", "check_config", "stop"])]
    overlay_demo: bool,
}

enum Outcome {
    Final(String),
    Recovered { text: String, error: String },
    Cancelled,
    Shutdown,
    Restart,
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

fn main() {
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
    if let Err(error) = run() {
        tracing::error!(error = %error, "operation failed");
        eprintln!("vime: {error:#}");
        std::process::exit(1);
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?)
}

fn run() -> Result<()> {
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
    if args.no_window {
        config.overlay.enabled = false;
    }
    if args.no_tray {
        config.tray.enabled = false;
    }
    config.validate()?;
    if args.check_config {
        println!("configuration is valid");
        return Ok(());
    }
    if args.test_connection {
        runtime()?.block_on(async {
            let mut connection = Connection::connect(&config.server).await?;
            connection.cancel().await
        })?;
        println!("connection and transcription session are ready");
        return Ok(());
    }
    if args.wav.is_some() {
        return runtime()?.block_on(run_wav(&args, &config));
    }
    #[cfg(not(windows))]
    if !args.overlay_demo {
        bail!("live mode requires a Windows user session");
    }
    run_desktop(args, config)
}

async fn run_wav(args: &Args, config: &Config) -> Result<()> {
    let samples = vime::audio::load_wav(args.wav.as_ref().unwrap())?;
    if samples.is_empty() {
        bail!("WAV input is empty");
    }
    let (_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
    let mut received = ReceivedText::default();
    let result = transcribe_wav(&samples, config, &mut controls, &mut received).await;
    let text = match recover_result(result, &received)? {
        Outcome::Final(text) => text,
        Outcome::Recovered { text, error } => {
            println!("{text}");
            vime::output::deliver_recovered(&text, &config.output).await?;
            bail!("{error}; received text was recovered and may be incomplete");
        }
        Outcome::Cancelled => bail!("recording was cancelled"),
        Outcome::Shutdown | Outcome::Restart => return Ok(()),
    };
    println!("{text}");
    if !text.trim().is_empty() {
        vime::output::deliver(&text, &config.output).await?;
    }
    Ok(())
}

fn run_desktop(args: Args, config: Config) -> Result<()> {
    if !config.overlay.enabled && !config.tray.enabled {
        let (ui_tx, _) = watch::channel(UiSnapshot::default());
        let progress = Progress(ui_tx);
        let (sender, mut controls) = tokio::sync::mpsc::unbounded_channel();
        return runtime()?.block_on(async {
            if args.overlay_demo {
                demo(&progress, &mut controls).await
            } else {
                run_live(&args, &config, &progress, sender, &mut controls).await
            }
        });
    }
    let (ui_tx, ui_rx) = watch::channel(UiSnapshot::default());
    let (desktop_tx, desktop_rx) = std::sync::mpsc::channel();
    let (tray_tx, mut tray_rx) = tokio::sync::mpsc::unbounded_channel();
    let _tray = vime::tray::start(
        config.tray.enabled,
        config.overlay.enabled,
        &config.hotkey.key,
        tray_tx.clone(),
    )?;
    let overlay_config = config.overlay.clone();
    let hotkey_config = config.hotkey.clone();
    let worker_desktop = desktop_tx.clone();
    let worker = std::thread::Builder::new()
        .name("vime-session".into())
        .spawn(move || {
            let result = (|| {
                runtime()?.block_on(async {
                    let (control_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
                    let hotkey_tx = control_tx.clone();
                    let desktop = worker_desktop.clone();
                    let bridge = tokio::spawn(async move {
                        while let Some(command) = tray_rx.recv().await {
                            match command {
                                TrayCommand::Restart => {
                                    let _ = control_tx.send(ControlEvent::Restart);
                                }
                                TrayCommand::Stop => {
                                    let _ = control_tx.send(ControlEvent::Shutdown);
                                }
                                TrayCommand::ToggleOverlay(enabled) => {
                                    let _ = desktop.send(DesktopCommand::Show(enabled));
                                }
                            }
                        }
                    });
                    let progress = Progress(ui_tx);
                    let result = if args.overlay_demo {
                        demo(&progress, &mut controls).await
                    } else {
                        run_live(&args, &config, &progress, hotkey_tx, &mut controls).await
                    };
                    bridge.abort();
                    result
                })
            })();
            let _ = worker_desktop.send(DesktopCommand::Exit);
            result
        })?;
    let gui = vime::overlay::run(
        ui_rx,
        &overlay_config,
        desktop_rx,
        &hotkey_config.key,
        hotkey_config.mode,
    );
    // Also stop the business thread if the native event loop exits unexpectedly.
    let _ = tray_tx.send(TrayCommand::Stop);
    let session = worker
        .join()
        .map_err(|_| anyhow::anyhow!("session thread panicked"))?;
    gui?;
    session
}

struct Progress(watch::Sender<UiSnapshot>);
impl Progress {
    fn start(&self) {
        self.0.send_replace(UiSnapshot {
            phase: UiPhase::Connecting,
            started: Some(std::time::Instant::now()),
            ..Default::default()
        });
    }
    fn phase(&self, phase: UiPhase, detail: &str) {
        self.0.send_modify(|state| {
            state.phase = phase;
            state.detail = detail.into();
        });
    }
    fn text(&self, received: &ReceivedText) {
        if self.0.receiver_count() == 0 {
            return;
        }
        if let Some(text) = received.recover() {
            self.0.send_modify(|state| state.text = text);
        }
    }
    fn finish(&self, phase: UiPhase, text: &str, detail: &str) {
        self.0.send_modify(|state| {
            state.phase = phase;
            state.text = text.into();
            state.detail = detail.into();
        });
    }
    fn idle(&self) {
        self.0.send_replace(UiSnapshot::default());
    }
}

async fn demo(progress: &Progress, controls: &mut UnboundedReceiver<ControlEvent>) -> Result<()> {
    progress.start();
    let started = tokio::time::Instant::now();
    let samples = [
        "我想让这个语音输入客户端", "我想让这个语音输入客户端，增加一个轻量的实时预览窗口。",
        "我想让这个语音输入客户端，增加一个轻量的实时预览窗口。\nHello, vime. 你好，世界！繁體中文測試 123。",
        "我想让这个语音输入客户端，增加一个轻量的实时预览窗口。\nHello, vime. 你好，世界！繁體中文測試 123。\n文字会随着识别持续增长，最后一句始终保持可见。",
    ];
    loop {
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_millis(100)) => {},
            event = controls.recv() => match event {
                Some(ControlEvent::Shutdown) | None => return Ok(()),
                Some(ControlEvent::Restart) => progress.start(),
                _ => {},
            }
        }
        let elapsed = started.elapsed().as_millis();
        if elapsed >= 10_000 {
            return Ok(());
        }
        match elapsed {
            0..=599 => {}
            600..=4599 => progress.finish(
                UiPhase::Recording,
                samples[((elapsed - 600) / 1000).min(3) as usize],
                "松开快捷键结束 · Esc 取消",
            ),
            4600..=5599 => progress.phase(UiPhase::Finalizing, "正在完成识别…"),
            5600..=6699 => progress.phase(UiPhase::Delivered, "预览：已复制到剪贴板"),
            6700..=7399 => progress.idle(),
            7400..=8499 => progress.finish(
                UiPhase::Recovered,
                "识别中断，保留已经收到的文字。",
                "预览：部分结果可能不完整",
            ),
            8500..=9399 => progress.finish(
                UiPhase::Error,
                "无法连接识别服务",
                "预览：请检查服务地址和网络",
            ),
            _ => progress.idle(),
        }
    }
}

async fn run_live(
    args: &Args,
    config: &Config,
    progress: &Progress,
    sender: tokio::sync::mpsc::UnboundedSender<ControlEvent>,
    controls: &mut UnboundedReceiver<ControlEvent>,
) -> Result<()> {
    let _instance = platform::SingleInstance::acquire()?;
    'restart: loop {
        while let Ok(event) = controls.try_recv() {
            if event == ControlEvent::Shutdown {
                break 'restart;
            }
        }
        let mut hide_at = None;
        let hotkey = platform::hotkey::start_into(&config.hotkey, sender.clone())?;
        progress.idle();
        tracing::info!("ready; configured hotkey starts recording");
        if args.foreground {
            eprintln!("vime is ready; Ctrl+C stops the process");
        }
        loop {
            tokio::select! {
                _ = console_interrupt() => break 'restart,
                _ = hide_deadline(hide_at) => { progress.idle(); hide_at = None; },
                event = controls.recv() => match event {
                    Some(ControlEvent::Start) => {
                        progress.start();
                        let result = record_live(config, args.device.as_deref(), controls, progress).await;
                        let recovered = matches!(&result, Ok(Outcome::Recovered { .. }));
                        match result {
                            Ok(Outcome::Final(text)) | Ok(Outcome::Recovered { text, .. }) => {
                                if let Some(action) = pending_cancel(controls) {
                                    match action { Outcome::Shutdown => break 'restart, Outcome::Restart => continue 'restart, _ => { progress.idle(); hotkey.reset()?; continue; } }
                                }
                                if text.trim().is_empty() { progress.finish(UiPhase::Delivered, "没有检测到语音", "可以开始下一次录音"); }
                                else {
                                    let delivery = async {
                                        if recovered { vime::output::deliver_recovered(&text, &config.output).await }
                                        else { vime::output::deliver(&text, &config.output).await }
                                    };
                                    tokio::pin!(delivery);
                                    let delivery = loop {
                                        tokio::select! {
                                            biased;
                                            event = controls.recv() => match event {
                                                Some(ControlEvent::Shutdown) | None => break 'restart,
                                                Some(ControlEvent::Restart) => continue 'restart,
                                                _ => {},
                                            },
                                            _ = console_interrupt() => break 'restart,
                                            result = &mut delivery => break result,
                                        }
                                    };
                                    match delivery {
                                        Ok(()) if recovered => { tracing::warn!("received text recovered; result may be incomplete"); progress.finish(UiPhase::Recovered, &text, "已交付部分文字 · 结果可能不完整"); },
                                        Ok(()) => { tracing::info!("final text delivered"); progress.finish(UiPhase::Delivered, &text, if config.output.clipboard { "已复制到剪贴板" } else { "识别完成" }); },
                                        Err(error) => { tracing::error!(error = %error, "delivery failed; not retried"); progress.finish(UiPhase::Error, &text, "交付失败 · 请查看日志"); }
                                    }
                                }
                            },
                            Ok(Outcome::Cancelled) => { tracing::info!("recording cancelled"); progress.idle(); },
                            Ok(Outcome::Shutdown) => break 'restart,
                            Ok(Outcome::Restart) => continue 'restart,
                            Err(error) => { tracing::error!(error = %error, "recording failed; ready for next recording"); progress.finish(UiPhase::Error, "无法完成识别", "请检查麦克风或服务连接 · 详情见日志"); },
                        }
                        hide_at = Some(tokio::time::Instant::now() + Duration::from_millis(if matches!(progress.0.borrow().phase, UiPhase::Recovered | UiPhase::Error) { 4000 } else { 1200 }));
                        hotkey.reset()?;
                        while let Ok(event) = controls.try_recv() {
                            match event { ControlEvent::Shutdown => break 'restart, ControlEvent::Restart => continue 'restart, _ => {} }
                        }
                    },
                    Some(ControlEvent::Shutdown) | None => break 'restart,
                    Some(ControlEvent::Restart) => continue 'restart,
                    _ => {},
                }
            }
        }
    }
    tracing::info!("stopped");
    Ok(())
}

async fn hide_deadline(deadline: Option<tokio::time::Instant>) {
    match deadline {
        Some(deadline) => tokio::time::sleep_until(deadline).await,
        None => std::future::pending().await,
    }
}

fn check_recording_event(
    event: ServerEvent,
    received: &mut ReceivedText,
    progress: Option<&Progress>,
) -> Result<()> {
    received.observe(&event)?;
    if let Some(progress) = progress {
        progress.text(received);
    }
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
    progress: &Progress,
) -> Result<Outcome> {
    let mut received = ReceivedText::default();
    let result = record_live_inner(config, device, controls, &mut received, progress).await;
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
            ControlEvent::Restart => return Some(Outcome::Restart),
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
                event = connection.next_event() => check_recording_event(event?, received, None)?,
            }
        }
    }
    connection.commit().await?;
    wait_final(
        &mut connection,
        controls,
        config.server.final_timeout_ms,
        received,
        None,
    )
    .await
}

async fn record_live_inner(
    config: &Config,
    device: Option<&str>,
    controls: &mut UnboundedReceiver<ControlEvent>,
    received: &mut ReceivedText,
    progress: &Progress,
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
                Some(ControlEvent::Restart) => return Ok(Outcome::Restart),
                _ => {},
            },
            _ = console_interrupt() => return Ok(Outcome::Shutdown),
            result = &mut connect => break result?,
        }
    };
    tracing::info!("recording");
    progress.phase(
        if released {
            UiPhase::Finalizing
        } else {
            UiPhase::Recording
        },
        "",
    );
    let mut sample_count = 0usize;
    loop {
        if released {
            break;
        }
        tokio::select! {
            biased;
            _ = console_interrupt() => return Ok(Outcome::Shutdown),
            event = controls.recv() => match event {
                Some(ControlEvent::Stop) => { capture.stop()?; progress.phase(UiPhase::Finalizing, "正在完成识别…"); break; },
                Some(ControlEvent::Cancel) => return Ok(Outcome::Cancelled),
                Some(ControlEvent::Shutdown) | None => return Ok(Outcome::Shutdown),
                Some(ControlEvent::Restart) => return Ok(Outcome::Restart),
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
            event = connection.next_event() => check_recording_event(event?, received, Some(progress))?,
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
                Some(ControlEvent::Restart) => return Ok(Outcome::Restart),
                    _ => {},
                },
                _ = console_interrupt() => return Ok(Outcome::Shutdown),
                result = &mut commit => { result?; break; },
            }
        }
    }
    tracing::info!("waiting for final text");
    progress.phase(UiPhase::Finalizing, "正在完成识别…");
    wait_final(
        &mut connection,
        controls,
        config.server.final_timeout_ms,
        received,
        Some(progress),
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
                    Some(ControlEvent::Restart) => return Ok(Some(Outcome::Restart)),
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
    progress: Option<&Progress>,
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
                    Some(ControlEvent::Restart) => return Ok(Outcome::Restart),
                    _ => {},
                },
                _ = console_interrupt() => return Ok(Outcome::Shutdown),
                _ = &mut deadline => bail!("final transcription timed out"),
                event = connection.next_event() => {
                    let event = event?;
                    received.observe(&event)?;
        if let Some(progress) = progress { progress.text(received); }
                    if let Some(text) = collector.observe(event)? {
                        // A confirmed final remains valid even if the peer cannot complete
                        // its closing handshake. Closing must not delay delivery indefinitely.
                        match tokio::time::timeout(Duration::from_millis(250), connection.close()).await {
                            Ok(Ok(())) => {},
                            _ => tracing::warn!("final received; WebSocket closing handshake incomplete"),
                        }
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
    async fn queued_cancel_shutdown_and_restart_precede_ready_final() {
        for event in [
            ControlEvent::Cancel,
            ControlEvent::Shutdown,
            ControlEvent::Restart,
        ] {
            let (mut connection, task) = connect_mock(Scenario::Success).await;
            connection.append(&[100; 480]).await.unwrap();
            connection.commit().await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
            let (tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
            tx.send(event).unwrap();
            let mut received = ReceivedText::default();
            let outcome = wait_final(&mut connection, &mut controls, 500, &mut received, None)
                .await
                .unwrap();
            match event {
                ControlEvent::Shutdown => assert!(matches!(outcome, Outcome::Shutdown)),
                ControlEvent::Restart => assert!(matches!(outcome, Outcome::Restart)),
                _ => assert!(matches!(outcome, Outcome::Cancelled)),
            }
            assert!(received.recover().is_none());
            task.abort();
        }
    }

    #[tokio::test]
    async fn preview_streams_partial_and_replaces_it_with_authoritative_final() {
        let (mut connection, task) = connect_mock(Scenario::Success).await;
        let (ui_tx, ui_rx) = watch::channel(UiSnapshot::default());
        let progress = Progress(ui_tx);
        progress.start();
        progress.phase(UiPhase::Recording, "");
        connection.append(&[100; 480]).await.unwrap();
        let mut received = ReceivedText::default();
        check_recording_event(
            connection.next_event().await.unwrap(),
            &mut received,
            Some(&progress),
        )
        .unwrap();
        assert_eq!(ui_rx.borrow().text, "你好，");
        assert_eq!(ui_rx.borrow().phase, UiPhase::Recording);
        connection.commit().await.unwrap();
        progress.phase(UiPhase::Finalizing, "正在完成识别…");
        let (_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let result = wait_final(
            &mut connection,
            &mut controls,
            500,
            &mut received,
            Some(&progress),
        )
        .await
        .unwrap();
        match result {
            Outcome::Final(text) => assert_eq!(ui_rx.borrow().text, text),
            _ => panic!("expected authoritative final"),
        }
        assert_eq!(ui_rx.borrow().text, "你好，世界。Hello, world!");
        assert_eq!(ui_rx.borrow().phase, UiPhase::Finalizing);
        progress.idle();
        assert!(ui_rx.borrow().text.is_empty());
        task.abort();
    }

    #[tokio::test]
    async fn timeout_recovers_partial_but_cancellation_does_not() {
        let (mut connection, task) = connect_mock(Scenario::Timeout).await;
        connection.append(&[100; 480]).await.unwrap();
        let mut received = ReceivedText::default();
        check_recording_event(connection.next_event().await.unwrap(), &mut received, None).unwrap();
        connection.commit().await.unwrap();
        let (_tx, mut controls) = tokio::sync::mpsc::unbounded_channel();
        let result = wait_final(&mut connection, &mut controls, 20, &mut received, None).await;
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
