use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use tokio_tungstenite::{connect_async, tungstenite::Message};
use vime::mock::CaptureReport;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn start(scenario: &str, directory: &std::path::Path) -> (Child, Socket) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_vime-mock"))
        .args([
            "--bind",
            "127.0.0.1:0",
            "--scenario",
            scenario,
            "--delay-ms",
            "150",
        ])
        .arg("--report")
        .arg(directory.join("report.json"))
        .arg("--capture-wav")
        .arg(directory.join("capture.wav"))
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let url = line.strip_prefix("LISTENING ").unwrap();
    let (socket, _) = connect_async(url).await.unwrap();
    (child, socket)
}
async fn send(socket: &mut Socket, event: Value) {
    socket
        .send(Message::Text(event.to_string().into()))
        .await
        .unwrap();
}
async fn receive(socket: &mut Socket) -> Value {
    let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(message.to_text().unwrap()).unwrap()
}
async fn prepare(socket: &mut Socket) {
    send(socket, json!({"type":"session.update","session":{"type":"transcription","audio":{"input":{"format":{"type":"audio/pcm","rate":24000},"turn_detection":null}}}})).await;
    assert_eq!(receive(socket).await["type"], "session.updated");
    send(socket, json!({"type":"input_audio_buffer.append","audio":STANDARD.encode([0u8, 0, 255, 127, 0, 128])})).await;
    assert_eq!(
        receive(socket).await["type"],
        "conversation.item.input_audio_transcription.delta"
    );
    send(socket, json!({"type":"input_audio_buffer.commit"})).await;
    let ack = receive(socket).await;
    assert_eq!(ack["type"], "input_audio_buffer.committed");
    assert_eq!(ack["item_id"], "mock-item-1");
}

#[tokio::test]
async fn independent_process_captures_pcm_and_emits_duplicate_finals() {
    let directory = tempfile::tempdir().unwrap();
    let (_child, mut socket) = start("duplicate", directory.path()).await;
    prepare(&mut socket).await;
    let final_event = receive(&mut socket).await;
    assert_eq!(
        final_event["type"],
        "conversation.item.input_audio_transcription.completed"
    );
    assert_eq!(final_event["transcript"], "你好，世界。Hello, world!");
    assert_eq!(receive(&mut socket).await, final_event);
    let report: CaptureReport =
        serde_json::from_slice(&std::fs::read(directory.path().join("report.json")).unwrap())
            .unwrap();
    assert!(report.session_valid);
    assert_eq!(report.sample_rate, 24000);
    assert_eq!(report.append_count, 1);
    assert_eq!(report.sample_count, 3);
    assert_eq!(report.commit_count, 1);
    let wav = hound::WavReader::open(directory.path().join("capture.wav")).unwrap();
    assert_eq!(wav.spec().sample_rate, 24000);
    assert_eq!(
        wav.into_samples::<i16>()
            .map(Result::unwrap)
            .collect::<Vec<_>>(),
        vec![0, 32767, -32768]
    );
}

#[tokio::test]
async fn independent_process_failure_scenarios() {
    for scenario in ["error", "disconnect", "timeout", "cancel"] {
        let directory = tempfile::tempdir().unwrap();
        let (_child, mut socket) = start(scenario, directory.path()).await;
        prepare(&mut socket).await;
        match scenario {
            "error" => assert_eq!(
                receive(&mut socket).await["error"]["message"],
                "simulated transcription failure"
            ),
            "disconnect" => {
                let message = tokio::time::timeout(Duration::from_secs(3), socket.next())
                    .await
                    .unwrap();
                assert!(matches!(message, Some(Ok(Message::Close(_))) | None));
            }
            "timeout" => assert!(
                tokio::time::timeout(Duration::from_millis(200), socket.next())
                    .await
                    .is_err()
            ),
            "cancel" => {
                send(&mut socket, json!({"type":"input_audio_buffer.clear"})).await;
                assert_eq!(
                    receive(&mut socket).await["type"],
                    "input_audio_buffer.cleared"
                );
                assert!(
                    tokio::time::timeout(Duration::from_millis(200), socket.next())
                        .await
                        .is_err()
                );
            }
            _ => unreachable!(),
        }
    }
}

#[tokio::test]
async fn clear_cancels_delayed_final_and_delayed_success_arrives() {
    let directory = tempfile::tempdir().unwrap();
    let (_child, mut socket) = start("delayed", directory.path()).await;
    prepare(&mut socket).await;
    send(&mut socket, json!({"type":"input_audio_buffer.clear"})).await;
    assert_eq!(
        receive(&mut socket).await["type"],
        "input_audio_buffer.cleared"
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(250), socket.next())
            .await
            .is_err()
    );
    let report: CaptureReport =
        serde_json::from_slice(&std::fs::read(directory.path().join("report.json")).unwrap())
            .unwrap();
    assert_eq!(report.clear_count, 1);
    let another_directory = tempfile::tempdir().unwrap();
    let (_other_child, mut other_socket) = start("delayed", another_directory.path()).await;
    prepare(&mut other_socket).await;
    assert_eq!(
        receive(&mut other_socket).await["type"],
        "conversation.item.input_audio_transcription.completed"
    );
}

#[tokio::test]
async fn rejects_wrong_audio_format() {
    let directory = tempfile::tempdir().unwrap();
    let (_child, mut socket) = start("success", directory.path()).await;
    send(&mut socket, json!({"type":"session.update","session":{"type":"transcription","audio":{"input":{"format":{"type":"audio/pcm","rate":16000},"turn_detection":null}}}})).await;
    assert_eq!(receive(&mut socket).await["type"], "error");
}
