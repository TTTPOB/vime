//! End-to-end WAV tests use two real executables and a real loopback socket.
use std::{
    path::{Path, PathBuf},
    process::{Output, Stdio},
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use vime::config::Config;

const TEXT: &str = "你好，Rust English $(touch unexpected) `literal` & %PATH%";

struct Fixture {
    directory: tempfile::TempDir,
    config: PathBuf,
    wav: PathBuf,
    log: PathBuf,
    transcript: PathBuf,
    calls: PathBuf,
    _mock: Child,
}
impl Fixture {
    async fn new(scenario: &str) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let mut mock = Command::new(env!("CARGO_BIN_EXE_vime-mock"))
            .args([
                "--bind",
                "127.0.0.1:0",
                "--scenario",
                scenario,
                "--delay-ms",
                "100",
                "--text",
                TEXT,
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let mut lines = BufReader::new(mock.stdout.take().unwrap()).lines();
        let line = tokio::time::timeout(Duration::from_secs(10), lines.next_line())
            .await
            .expect("mock did not become ready")
            .unwrap()
            .expect("mock exited before ready");
        let url = line
            .strip_prefix("LISTENING ")
            .expect("mock readiness line")
            .to_owned();
        let transcript = root.join("transcript.txt");
        let calls = root.join("calls.txt");
        let command = output_script(root, &transcript, &calls);
        let mut configuration = Config::default();
        configuration.server.url = url;
        configuration.server.connect_timeout_ms = 2_000;
        configuration.server.final_timeout_ms = if scenario == "timeout" { 200 } else { 2_000 };
        configuration.output.command = command;
        configuration.output.script_timeout_ms = 10_000;
        // --no-clipboard must override this; headless Linux would otherwise fail.
        configuration.output.clipboard = true;
        let config = root.join("vime.toml");
        std::fs::write(&config, toml::to_string(&configuration).unwrap()).unwrap();
        let wav = root.join("input.wav");
        let mut writer = hound::WavWriter::create(
            &wav,
            hound::WavSpec {
                channels: 1,
                sample_rate: 24_000,
                bits_per_sample: 16,
                sample_format: hound::SampleFormat::Int,
            },
        )
        .unwrap();
        for index in 0..2_400 {
            let phase = index as f64 * std::f64::consts::TAU * 440.0 / 24_000.0;
            writer.write_sample((phase.sin() * 8_000.0) as i16).unwrap();
        }
        writer.finalize().unwrap();
        let log = root.join("vime.log");
        Self {
            directory,
            config,
            wav,
            log,
            transcript,
            calls,
            _mock: mock,
        }
    }
    async fn run(&self, args: &[&str]) -> Output {
        let child = Command::new(env!("CARGO_BIN_EXE_vime"))
            .arg("--config")
            .arg(&self.config)
            .arg("--no-clipboard")
            .arg("--log-file")
            .arg(&self.log)
            .args(args)
            .current_dir(self.directory.path())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(15), child.wait_with_output())
            .await
            .expect("vime exceeded absolute process deadline")
            .unwrap()
    }
    async fn transcribe(&self) -> Output {
        self.run(&["--wav", self.wav.to_str().unwrap()]).await
    }
}

#[cfg(unix)]
fn output_script(root: &Path, transcript: &Path, calls: &Path) -> Vec<String> {
    let script = root.join("output.sh");
    std::fs::write(
        &script,
        "#!/bin/sh\nset -eu\ncat > \"$1\"\nprintf '1 %s\\n' \"$VIME_TRANSCRIPT_STATUS\" >> \"$2\"\n",
    )
    .unwrap();
    vec![
        "sh".into(),
        script.to_string_lossy().into(),
        transcript.to_string_lossy().into(),
        calls.to_string_lossy().into(),
    ]
}
#[cfg(windows)]
fn output_script(root: &Path, transcript: &Path, calls: &Path) -> Vec<String> {
    let script = root.join("output.ps1");
    std::fs::write(
        &script,
        r#"param([string]$Target, [string]$Calls)
$ErrorActionPreference = 'Stop'
[Console]::InputEncoding = New-Object System.Text.UTF8Encoding($false)
$text = [Console]::In.ReadToEnd()
$utf8 = New-Object System.Text.UTF8Encoding($false)
[IO.File]::WriteAllText($Target, $text, $utf8)
[IO.File]::AppendAllText($Calls, "1 ${env:VIME_TRANSCRIPT_STATUS}`n", $utf8)
"#,
    )
    .unwrap();
    vec![
        "powershell.exe".into(),
        "-NoLogo".into(),
        "-NoProfile".into(),
        "-NonInteractive".into(),
        "-ExecutionPolicy".into(),
        "Bypass".into(),
        "-File".into(),
        script.to_string_lossy().into(),
        transcript.to_string_lossy().into(),
        calls.to_string_lossy().into(),
    ]
}

#[tokio::test]
async fn wav_success_delayed_duplicate_deliver_literal_final_once() {
    tokio::time::timeout(Duration::from_secs(50), async {
        for scenario in ["success", "delayed", "duplicate"] {
            let fixture = Fixture::new(scenario).await;
            let output = fixture.transcribe().await;
            assert!(
                output.status.success(),
                "{scenario}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            assert_eq!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .trim_end_matches(['\r', '\n']),
                TEXT,
                "{scenario}"
            );
            assert_eq!(
                std::fs::read_to_string(&fixture.transcript).unwrap(),
                TEXT,
                "{scenario}: stdin changed"
            );
            assert_eq!(
                std::fs::read_to_string(&fixture.calls).unwrap(),
                "1 final\n",
                "{scenario}: output called more than once"
            );
            assert!(!fixture.directory.path().join("unexpected").exists());
            assert!(fixture.log.exists());
        }
    })
    .await
    .expect("success scenario suite exceeded absolute deadline");
}

#[tokio::test]
async fn failed_transcriptions_recover_received_text_once_and_report_failure() {
    tokio::time::timeout(Duration::from_secs(45), async {
        for scenario in ["error", "disconnect", "timeout"] {
            let fixture = Fixture::new(scenario).await;
            let output = fixture.transcribe().await;
            assert!(
                !output.status.success(),
                "{scenario} unexpectedly succeeded"
            );
            assert_eq!(
                String::from_utf8(output.stdout)
                    .unwrap()
                    .trim_end_matches(['\r', '\n']),
                "你好，",
                "{scenario}: received text was lost"
            );
            assert_eq!(
                std::fs::read_to_string(&fixture.transcript).unwrap(),
                "你好，"
            );
            assert_eq!(
                std::fs::read_to_string(&fixture.calls).unwrap(),
                "1 recovered\n",
                "{scenario}: recovery must run once with recovered status"
            );
            assert!(fixture.log.exists());
            let log = std::fs::read_to_string(&fixture.log).unwrap();
            assert!(
                log.contains("operation failed"),
                "{scenario} missing error log: {log}"
            );
        }
    })
    .await
    .expect("failure scenario suite exceeded absolute deadline");
}

#[tokio::test]
async fn cli_validates_configuration_and_checks_real_session() {
    tokio::time::timeout(Duration::from_secs(35), async {
        let fixture = Fixture::new("success").await;
        let check = fixture.run(&["--check-config"]).await;
        assert!(
            check.status.success(),
            "{}",
            String::from_utf8_lossy(&check.stderr)
        );
        assert!(String::from_utf8(check.stdout)
            .unwrap()
            .contains("configuration is valid"));
        let connection = fixture.run(&["--test-connection"]).await;
        assert!(
            connection.status.success(),
            "{}",
            String::from_utf8_lossy(&connection.stderr)
        );
        assert!(String::from_utf8(connection.stdout)
            .unwrap()
            .contains("transcription session are ready"));
        assert!(!fixture.calls.exists());
        std::fs::write(
            &fixture.config,
            "[server]\nurl = 'https://invalid.example'\n",
        )
        .unwrap();
        let invalid = fixture.run(&["--check-config"]).await;
        assert!(!invalid.status.success());
        assert!(invalid.stdout.is_empty());
    })
    .await
    .expect("configuration check suite exceeded absolute deadline");
}

#[tokio::test]
async fn recovery_can_use_an_independent_command_or_disable_scripts() {
    tokio::time::timeout(Duration::from_secs(40), async {
        let fixture = Fixture::new("error").await;
        let recovered = fixture.directory.path().join("recovered.txt");
        let recovery_calls = fixture.directory.path().join("recovery-calls.txt");
        let mut config = Config::load(&fixture.config).unwrap();
        config.output.recovery_command = Some(output_script(
            fixture.directory.path(),
            &recovered,
            &recovery_calls,
        ));
        std::fs::write(&fixture.config, toml::to_string(&config).unwrap()).unwrap();
        let result = fixture.transcribe().await;
        assert!(!result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout)
                .unwrap()
                .trim_end_matches(['\r', '\n']),
            "你好，"
        );
        assert_eq!(std::fs::read_to_string(recovered).unwrap(), "你好，");
        assert_eq!(
            std::fs::read_to_string(recovery_calls).unwrap(),
            "1 recovered\n"
        );
        assert!(!fixture.transcript.exists());
        assert!(!fixture.calls.exists());

        let disabled = Fixture::new("timeout").await;
        let mut config = Config::load(&disabled.config).unwrap();
        config.output.recovery_command = Some(vec![]);
        std::fs::write(&disabled.config, toml::to_string(&config).unwrap()).unwrap();
        let result = disabled.transcribe().await;
        assert!(!result.status.success());
        assert_eq!(
            String::from_utf8(result.stdout)
                .unwrap()
                .trim_end_matches(['\r', '\n']),
            "你好，"
        );
        assert!(!disabled.transcript.exists());
        assert!(!disabled.calls.exists());
    })
    .await
    .expect("recovery command suite exceeded absolute deadline");
}
