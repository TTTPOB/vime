use base64::{engine::general_purpose::STANDARD, Engine};
use futures_util::{SinkExt, StreamExt};
use serde_json::{json, Value};
use tokio::net::TcpListener;
use tokio_tungstenite::{accept_async, tungstenite::Message};
use vime::{
    config::ServerConfig,
    protocol::{Connection, ServerEvent, MAX_AUDIO_SAMPLES},
};

fn config(url: String) -> ServerConfig {
    ServerConfig {
        url,
        model: "mock-asr".into(),
        api_key_env: None,
        connect_timeout_ms: 300,
        final_timeout_ms: 300,
    }
}

async fn listener() -> (TcpListener, ServerConfig) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let config = config(format!(
        "ws://{}/custom-transcription",
        listener.local_addr().unwrap()
    ));
    (listener, config)
}

#[tokio::test]
async fn real_socket_audio_commit_and_final() {
    let (listener, config) = listener().await;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let update: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(
            update["session"]["audio"]["input"]["turn_detection"],
            Value::Null
        );
        ws.send(Message::text(
            json!({"type":"session.updated", "session":update["session"]}).to_string(),
        ))
        .await
        .unwrap();
        let append: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(
            STANDARD.decode(append["audio"].as_str().unwrap()).unwrap(),
            [0, 128, 255, 127, 42, 0]
        );
        let commit: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(commit["type"], "input_audio_buffer.commit");
        for event in [
            json!({"type":"input_audio_buffer.committed","item_id":"one"}),
            json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"one","delta":"he"}),
            json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"one","transcript":"hello"}),
        ] {
            ws.send(Message::text(event.to_string())).await.unwrap();
        }
    });
    let mut connection = Connection::connect(&config).await.unwrap();
    assert!(connection
        .append(&vec![0; MAX_AUDIO_SAMPLES + 1])
        .await
        .is_err());
    connection.append(&[i16::MIN, i16::MAX, 42]).await.unwrap();
    connection.commit().await.unwrap();
    assert!(connection.commit().await.is_err());
    assert_eq!(
        connection.next_event().await.unwrap(),
        ServerEvent::Committed {
            item_id: "one".into()
        }
    );
    assert_eq!(
        connection.next_event().await.unwrap(),
        ServerEvent::Delta {
            item_id: "one".into(),
            text: "he".into()
        }
    );
    assert_eq!(
        connection.next_event().await.unwrap(),
        ServerEvent::Final {
            item_id: "one".into(),
            text: "hello".into()
        }
    );
    task.await.unwrap();
}

#[tokio::test]
async fn rejects_wrong_session_format() {
    let (listener, config) = listener().await;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        ws.next().await.unwrap().unwrap();
        ws.send(Message::text(
            json!({"type":"session.updated","session":{"type":"transcription",
            "audio":{"input":{"format":{"type":"audio/pcm","rate":16000},"turn_detection":null}}}})
            .to_string(),
        ))
        .await
        .unwrap();
    });
    assert!(Connection::connect(&config).await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn readiness_timeout_is_bounded() {
    let (listener, mut config) = listener().await;
    config.connect_timeout_ms = 50;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        ws.next().await.unwrap().unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    });
    let error = Connection::connect(&config).await.err().unwrap();
    assert!(error.to_string().contains("readiness timed out"));
    task.abort();
}

#[tokio::test]
// Tungstenite's public handshake callback requires its large HTTP response error type.
#[allow(clippy::result_large_err)]
async fn uses_exact_custom_path_and_configured_auth() {
    let (listener, mut config) = listener().await;
    let name = format!("VIME_PROTOCOL_TEST_KEY_{}", std::process::id());
    std::env::set_var(&name, "mock-secret-value");
    config.api_key_env = Some(name.clone());
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = tokio_tungstenite::accept_hdr_async(
            stream,
            |request: &tokio_tungstenite::tungstenite::handshake::server::Request, response| {
                assert_eq!(request.uri().path(), "/custom-transcription");
                assert_eq!(
                    request.headers()["Authorization"],
                    "Bearer mock-secret-value"
                );
                Ok(response)
            },
        )
        .await
        .unwrap();
        let update: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        ws.send(Message::text(
            json!({"type":"session.updated","session":update["session"]}).to_string(),
        ))
        .await
        .unwrap();
        ws.send(Message::text(
            json!({"type":"error","error":{"message":"mock-secret-value"}}).to_string(),
        ))
        .await
        .unwrap();
    });
    let mut connection = Connection::connect(&config).await.unwrap();
    std::env::remove_var(name);
    let event = connection.next_event().await.unwrap();
    assert!(matches!(&event, ServerEvent::Error { .. }));
    assert!(!format!("{event:?}").contains("mock-secret-value"));
    task.await.unwrap();
}

#[tokio::test]
async fn malformed_final_is_an_error() {
    let (listener, config) = listener().await;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let update: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        ws.send(Message::text(
            json!({"type":"session.updated","session":update["session"]}).to_string(),
        ))
        .await
        .unwrap();
        ws.send(Message::text(json!({"type":"conversation.item.input_audio_transcription.completed","transcript":"missing item"}).to_string())).await.unwrap();
    });
    let mut connection = Connection::connect(&config).await.unwrap();
    assert!(connection.next_event().await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn oversized_server_message_is_rejected() {
    let (listener, config) = listener().await;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let update: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        ws.send(Message::text(
            json!({"type":"session.updated","session":update["session"]}).to_string(),
        ))
        .await
        .unwrap();
        let _ = ws
            .send(Message::text(
                "x".repeat(vime::protocol::MAX_MESSAGE_BYTES + 1),
            ))
            .await;
    });
    let mut connection = Connection::connect(&config).await.unwrap();
    assert!(connection.next_event().await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn cancel_clears_and_prevents_late_output() {
    let (listener, config) = listener().await;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let update: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        ws.send(Message::text(
            json!({"type":"session.updated","session":update["session"]}).to_string(),
        ))
        .await
        .unwrap();
        let clear: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(clear["type"], "input_audio_buffer.clear");
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
    });
    let mut connection = Connection::connect(&config).await.unwrap();
    connection.cancel().await.unwrap();
    assert!(connection.next_event().await.is_err());
    assert!(connection.append(&[1]).await.is_err());
    task.await.unwrap();
}

#[tokio::test]
async fn rejects_untrusted_wss_certificate() {
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;
    let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    let tls_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![certificate.cert.der().clone()],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(
            certificate.signing_key.serialize_der(),
        )),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mut config = config(format!(
        "wss://localhost:{}/transcribe",
        listener.local_addr().unwrap().port()
    ));
    config.connect_timeout_ms = 3000;
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        acceptor.accept(stream).await.is_err()
    });
    assert!(Connection::connect(&config).await.is_err());
    assert!(task.await.unwrap());
}

#[tokio::test]
async fn trusted_wss_works_through_production_cli() {
    use rcgen::{
        BasicConstraints, CertificateParams, CertifiedIssuer, ExtendedKeyUsagePurpose, IsCa,
        KeyPair, KeyUsagePurpose,
    };
    use rustls::pki_types::{PrivateKeyDer, PrivatePkcs8KeyDer};
    use std::sync::Arc;

    let mut ca_params = CertificateParams::new(Vec::<String>::new()).unwrap();
    ca_params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    ca_params.key_usages = vec![KeyUsagePurpose::KeyCertSign, KeyUsagePurpose::CrlSign];
    let ca = CertifiedIssuer::self_signed(ca_params, KeyPair::generate().unwrap()).unwrap();
    let leaf_key = KeyPair::generate().unwrap();
    let mut leaf_params = CertificateParams::new(vec!["localhost".into()]).unwrap();
    leaf_params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    leaf_params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];
    let leaf = leaf_params.signed_by(&leaf_key, &ca).unwrap();
    let tls_config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(
        vec![leaf.der().clone(), ca.der().clone()],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(leaf_key.serialize_der())),
    )
    .unwrap();
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(tls_config));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let task = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let stream = acceptor.accept(stream).await.unwrap();
        let mut ws = accept_async(stream).await.unwrap();
        let update: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(update["type"], "session.update");
        ws.send(Message::text(
            json!({"type":"session.updated","session":update["session"]}).to_string(),
        ))
        .await
        .unwrap();
        let clear: Value =
            serde_json::from_str(ws.next().await.unwrap().unwrap().to_text().unwrap()).unwrap();
        assert_eq!(clear["type"], "input_audio_buffer.clear");
        assert!(matches!(
            ws.next().await.unwrap().unwrap(),
            Message::Close(_)
        ));
    });
    let dir = tempfile::tempdir().unwrap();
    let ca_path = dir.path().join("test-ca.pem");
    let config_path = dir.path().join("vime.toml");
    std::fs::write(&ca_path, ca.pem()).unwrap();
    std::fs::write(
        &config_path,
        format!(
            "[server]\nurl = 'wss://localhost:{port}/transcription'\nconnect_timeout_ms = 5000\n"
        ),
    )
    .unwrap();
    // The CA override belongs only to this child. Never mutate the system trust store
    // or the test process environment (other tests must still reject this CA).
    let output = tokio::time::timeout(
        std::time::Duration::from_secs(10),
        tokio::process::Command::new(env!("CARGO_BIN_EXE_vime"))
            .arg("--config")
            .arg(&config_path)
            .arg("--test-connection")
            .env("SSL_CERT_FILE", &ca_path)
            .env_remove("SSL_CERT_DIR")
            .current_dir(dir.path())
            .kill_on_drop(true)
            .output(),
    )
    .await
    .unwrap()
    .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    task.await.unwrap();
}
