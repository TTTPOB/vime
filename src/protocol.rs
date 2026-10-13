//! Deliberately small Realtime-inspired transcription subset, not full API compatibility.
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::net::TcpStream;
use tokio_tungstenite::{
    connect_async_with_config,
    tungstenite::{
        client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig, Message,
    },
    MaybeTlsStream, WebSocketStream,
};

use crate::config::ServerConfig;

pub const SAMPLE_RATE: u32 = 24_000;
pub const MAX_AUDIO_SAMPLES: usize = 24_000;
pub const MAX_MESSAGE_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerEvent {
    Delta { item_id: String, text: String },
    Committed { item_id: String },
    Final { item_id: String, text: String },
    Error { message: String },
    Other,
}

pub struct Connection {
    socket: WebSocketStream<MaybeTlsStream<TcpStream>>,
    io_timeout: Duration,
    committed: bool,
    cancelled: bool,
}

impl Connection {
    pub async fn connect(config: &ServerConfig) -> Result<Self> {
        let url = url::Url::parse(&config.url).context("invalid ASR URL")?;
        if !matches!(url.scheme(), "ws" | "wss") {
            bail!("ASR URL must use ws or wss");
        }
        if !url.username().is_empty() || url.password().is_some() {
            bail!("ASR URL must not contain credentials; use api_key_env");
        }
        let mut request = config
            .url
            .as_str()
            .into_client_request()
            .map_err(|_| anyhow!("cannot construct ASR connection request"))?;
        if let Some(name) = &config.api_key_env {
            let key = std::env::var(name).map_err(|_| {
                anyhow!("configured ASR API key environment variable is unavailable")
            })?;
            if key.trim().is_empty() {
                bail!("configured ASR API key is empty");
            }
            let mut value = HeaderValue::from_str(&format!("Bearer {key}"))
                .map_err(|_| anyhow!("configured ASR API key is not a valid header value"))?;
            value.set_sensitive(true);
            request.headers_mut().insert("Authorization", value);
        }
        let io_timeout = Duration::from_millis(config.connect_timeout_ms);
        let ws_config = WebSocketConfig::default()
            .max_message_size(Some(MAX_MESSAGE_BYTES))
            .max_frame_size(Some(MAX_MESSAGE_BYTES));
        // Rustls uses normal native-root validation. No insecure certificate bypass.
        let (socket, _) = tokio::time::timeout(
            io_timeout,
            connect_async_with_config(request, Some(ws_config), false),
        )
        .await
        .context("ASR connection timed out")?
        .map_err(|_| anyhow!("ASR WebSocket/TLS connection failed"))?;
        let mut connection = Self {
            socket,
            io_timeout,
            committed: false,
            cancelled: false,
        };
        connection
            .send(json!({
                "type": "session.update",
                "session": {
                    "type": "transcription",
                    "audio": {"input": {
                        "format": {"type": "audio/pcm", "rate": SAMPLE_RATE},
                        "transcription": {"model": config.model},
                        "turn_detection": null
                    }}
                }
            }))
            .await?;
        tokio::time::timeout(io_timeout, async {
            loop {
                let value = connection.receive_json().await?;
                match value.get("type").and_then(Value::as_str) {
                    Some("session.updated") => {
                        validate_session(&value)?;
                        return Ok::<(), anyhow::Error>(());
                    }
                    Some("error") => bail!("ASR rejected transcription session"),
                    Some("session.created") => {}
                    _ => bail!("unexpected event before transcription session was ready"),
                }
            }
        })
        .await
        .context("ASR session readiness timed out")??;
        Ok(connection)
    }

    pub async fn append(&mut self, samples: &[i16]) -> Result<()> {
        if self.committed || self.cancelled {
            bail!("recording is already finished");
        }
        if samples.is_empty() || samples.len() > MAX_AUDIO_SAMPLES {
            bail!("audio chunk must contain 1 to {MAX_AUDIO_SAMPLES} PCM16 samples");
        }
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        self.send(json!({"type": "input_audio_buffer.append", "audio": STANDARD.encode(bytes)}))
            .await
    }

    pub async fn commit(&mut self) -> Result<()> {
        if self.committed || self.cancelled {
            bail!("recording is already finished");
        }
        self.send(json!({"type": "input_audio_buffer.commit"}))
            .await?;
        self.committed = true;
        Ok(())
    }

    pub async fn cancel(&mut self) -> Result<()> {
        if self.cancelled {
            return Ok(());
        }
        self.cancelled = true;
        // clear removes uncommitted audio; closing isolates already committed results.
        let result = self.send(json!({"type": "input_audio_buffer.clear"})).await;
        let close_result = tokio::time::timeout(self.io_timeout, self.socket.close(None)).await;
        result?;
        close_result
            .context("ASR close timed out")?
            .map_err(|_| anyhow!("ASR close failed"))?;
        Ok(())
    }

    pub async fn next_event(&mut self) -> Result<ServerEvent> {
        if self.cancelled {
            bail!("recording was cancelled");
        }
        // The caller owns the absolute post-commit deadline; long recordings are valid.
        let value = self.receive_json().await?;
        parse_event(value)
    }

    pub async fn close(&mut self) -> Result<()> {
        tokio::time::timeout(self.io_timeout, self.socket.close(None))
            .await
            .context("ASR close timed out")?
            .map_err(|_| anyhow!("ASR close failed"))
    }

    async fn send(&mut self, value: Value) -> Result<()> {
        let text = serde_json::to_string(&value)?;
        if text.len() > MAX_MESSAGE_BYTES {
            bail!("ASR message exceeds size limit");
        }
        tokio::time::timeout(
            self.io_timeout,
            self.socket.send(Message::Text(text.into())),
        )
        .await
        .context("ASR send timed out")?
        .map_err(|_| anyhow!("ASR WebSocket send failed"))
    }

    async fn receive_json(&mut self) -> Result<Value> {
        loop {
            let message = self
                .socket
                .next()
                .await
                .ok_or_else(|| anyhow!("ASR disconnected before a complete result"))?
                .map_err(|_| anyhow!("ASR WebSocket receive failed"))?;
            match message {
                Message::Text(text) => {
                    return serde_json::from_str(&text).context("ASR sent invalid JSON");
                }
                Message::Ping(payload) => {
                    tokio::time::timeout(self.io_timeout, self.socket.send(Message::Pong(payload)))
                        .await
                        .context("ASR pong timed out")?
                        .map_err(|_| anyhow!("ASR pong failed"))?;
                }
                Message::Pong(_) => {}
                Message::Close(_) => bail!("ASR disconnected before a complete result"),
                _ => bail!("ASR sent an unsupported binary message"),
            }
        }
    }
}

fn validate_session(value: &Value) -> Result<()> {
    let session = &value["session"];
    let input = &session["audio"]["input"];
    if session["type"] != "transcription"
        || input["format"]["type"] != "audio/pcm"
        || input["format"]["rate"].as_u64() != Some(SAMPLE_RATE as u64)
        || !input
            .as_object()
            .is_some_and(|o| o.get("turn_detection") == Some(&Value::Null))
    {
        bail!("ASR session must confirm transcription, 24kHz PCM and disabled VAD");
    }
    Ok(())
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value[field]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("ASR event is missing string field {field}"))
}

fn parse_event(value: Value) -> Result<ServerEvent> {
    let kind = required_string(&value, "type")?;
    Ok(match kind.as_str() {
        "input_audio_buffer.committed" => ServerEvent::Committed {
            item_id: required_string(&value, "item_id")?,
        },
        "conversation.item.input_audio_transcription.delta" => ServerEvent::Delta {
            item_id: required_string(&value, "item_id")?,
            text: required_string(&value, "delta")?,
        },
        "conversation.item.input_audio_transcription.completed" => ServerEvent::Final {
            item_id: required_string(&value, "item_id")?,
            text: required_string(&value, "transcript")?,
        },
        "error" | "conversation.item.input_audio_transcription.failed" => ServerEvent::Error {
            // Do not reflect untrusted server messages which may echo credentials.
            message: "ASR reported a transcription error".to_owned(),
        },
        _ => ServerEvent::Other,
    })
}
