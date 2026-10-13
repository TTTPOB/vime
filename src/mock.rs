//! A deterministic protocol simulator. It does not recognize speech.
use anyhow::{bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use clap::ValueEnum;
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{net::SocketAddr, path::PathBuf, time::Duration};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{accept_async, tungstenite::Message, WebSocketStream};

#[derive(Debug, Clone, Copy, ValueEnum, PartialEq, Eq)]
pub enum Scenario {
    Success,
    Duplicate,
    Error,
    Disconnect,
    Timeout,
    Delayed,
    Cancel,
}

#[derive(Debug, Clone)]
pub struct MockOptions {
    pub bind: SocketAddr,
    pub text: String,
    pub scenario: Scenario,
    pub delay_ms: u64,
    pub report: Option<PathBuf>,
    pub capture_wav: Option<PathBuf>,
}
impl Default for MockOptions {
    fn default() -> Self {
        Self {
            bind: "127.0.0.1:8765".parse().unwrap(),
            text: "你好，世界。Hello, world!".into(),
            scenario: Scenario::Success,
            delay_ms: 250,
            report: None,
            capture_wav: None,
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct CaptureReport {
    pub session_valid: bool,
    pub sample_rate: u32,
    pub append_count: usize,
    pub sample_count: usize,
    pub commit_count: usize,
    pub clear_count: usize,
    pub validation_error: Option<String>,
}

/// Serve already-bound listener; callers can discover its ephemeral port first.
pub async fn serve(listener: TcpListener, options: MockOptions) -> Result<()> {
    loop {
        let (stream, _) = listener.accept().await?;
        let options = options.clone();
        tokio::spawn(async move {
            if let Err(error) = handle_connection(stream, &options).await {
                eprintln!("mock connection: {error:#}");
            }
        });
    }
}

async fn send(ws: &mut WebSocketStream<TcpStream>, event: Value) -> Result<()> {
    ws.send(Message::Text(event.to_string().into())).await?;
    Ok(())
}

fn save(options: &MockOptions, report: &CaptureReport, samples: &[i16]) -> Result<()> {
    if let Some(path) = &options.report {
        std::fs::write(path, serde_json::to_vec_pretty(report)?)?;
    }
    if let Some(path) = &options.capture_wav {
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 24000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut wav = hound::WavWriter::create(path, spec)?;
        for sample in samples {
            wav.write_sample(*sample)?;
        }
        wav.finalize()?;
    }
    Ok(())
}

async fn handle_connection(stream: TcpStream, options: &MockOptions) -> Result<()> {
    let mut ws = accept_async(stream).await?;
    let mut report = CaptureReport::default();
    let mut samples = Vec::new();
    let result = conversation(&mut ws, options, &mut report, &mut samples).await;
    if let Err(error) = &result {
        report.validation_error = Some(format!("{error:#}"));
    }
    save(options, &report, &samples)?;
    result
}

async fn conversation(
    ws: &mut WebSocketStream<TcpStream>,
    options: &MockOptions,
    report: &mut CaptureReport,
    samples: &mut Vec<i16>,
) -> Result<()> {
    'messages: while let Some(message) = ws.next().await {
        let message = message?;
        let text = match message {
            Message::Text(text) => text,
            Message::Ping(payload) => {
                ws.send(Message::Pong(payload)).await?;
                continue;
            }
            Message::Close(_) => {
                // Reading Close queues Tungstenite's reply; flush it before
                // dropping the transport so a normal session ends cleanly.
                ws.flush().await?;
                break;
            }
            _ => continue,
        };
        let event: Value = serde_json::from_str(&text).context("invalid client JSON")?;
        let event_type = event["type"].as_str().unwrap_or_default();
        match event_type {
            "session.update" => {
                let session = &event["session"];
                let input = &session["audio"]["input"];
                if session["type"] != "transcription"
                    || input["format"]["type"] != "audio/pcm"
                    || input["format"]["rate"] != 24000
                    || !input.get("turn_detection").is_some_and(Value::is_null)
                {
                    send(ws, json!({"type":"error","error":{"message":"expected transcription session, PCM16 24000 Hz, turn_detection null"}})).await?;
                    bail!("invalid session configuration");
                }
                report.session_valid = true;
                report.sample_rate = 24000;
                send(ws, json!({"type":"session.updated","session":session})).await?;
            }
            "input_audio_buffer.append" => {
                if !report.session_valid {
                    bail!("audio before session.update");
                }
                let data = STANDARD
                    .decode(event["audio"].as_str().context("missing base64 audio")?)
                    .context("invalid base64 audio")?;
                if data.len() % 2 != 0 {
                    bail!("PCM16 data has odd byte count");
                }
                if samples.len() + data.len() / 2 > 32 * 1024 * 1024 {
                    bail!("audio exceeds mock capture limit");
                }
                samples.extend(
                    data.as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| i16::from_le_bytes([b[0], b[1]])),
                );
                report.append_count += 1;
                report.sample_count = samples.len();
                if report.append_count == 1 {
                    send(ws, json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"mock-item-1","delta":"你好，"})).await?;
                }
            }
            "input_audio_buffer.clear" => {
                report.clear_count += 1;
                save(options, report, samples)?;
                send(ws, json!({"type":"input_audio_buffer.cleared"})).await?;
                // Cancellation never emits a final result.
            }
            "input_audio_buffer.commit" => {
                if !report.session_valid {
                    bail!("commit before session.update");
                }
                report.commit_count += 1;
                save(options, report, samples)?;
                send(
                    ws,
                    json!({"type":"input_audio_buffer.committed","item_id":"mock-item-1"}),
                )
                .await?;
                match options.scenario {
                    Scenario::Timeout | Scenario::Cancel => {}
                    Scenario::Disconnect => {
                        ws.close(None).await?;
                        break;
                    }
                    Scenario::Error => {
                        send(ws, json!({"type":"error","error":{"code":"mock_error","message":"simulated transcription failure"}})).await?;
                    }
                    scenario => {
                        if scenario == Scenario::Delayed {
                            let deadline =
                                tokio::time::sleep(Duration::from_millis(options.delay_ms));
                            tokio::pin!(deadline);
                            loop {
                                tokio::select! {
                                    _ = &mut deadline => break,
                                    message = ws.next() => {
                                        match message {
                                            Some(Ok(Message::Text(text))) => {
                                                let event: Value = serde_json::from_str(&text)?;
                                                if event["type"] == "input_audio_buffer.clear" {
                                                    report.clear_count += 1;
                                                    save(options, report, samples)?;
                                                    send(ws, json!({"type":"input_audio_buffer.cleared"})).await?;
                                                    continue 'messages;
                                                }
                                            }
                                            Some(Ok(Message::Ping(payload))) => ws.send(Message::Pong(payload)).await?,
                                            Some(Ok(Message::Close(_))) | None => return Ok(()),
                                            Some(Err(error)) => return Err(error.into()),
                                            _ => {},
                                        }
                                    }
                                }
                            }
                        }
                        let final_event = json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"mock-item-1","transcript":options.text});
                        send(ws, final_event.clone()).await?;
                        if scenario == Scenario::Duplicate {
                            send(ws, final_event).await?;
                        }
                    }
                }
            }
            _ => {
                send(ws, json!({"type":"error","error":{"message":format!("unsupported event: {event_type}")}})).await?;
            }
        }
    }
    Ok(())
}
