use crate::config::OutputConfig;
use anyhow::{bail, Context, Result};
use std::{process::Stdio, time::Duration};
use tokio::{io::AsyncWriteExt, process::Command};

/// A successful final is delivered once by the caller. Clipboard is updated first;
/// a script failure does not roll back a successful clipboard write.
pub async fn deliver(text: &str, config: &OutputConfig) -> Result<()> {
    deliver_with_status(text, config, &config.command, "final").await
}

/// Deliver best-effort text after a recording failure. Never called for cancellation.
pub async fn deliver_recovered(text: &str, config: &OutputConfig) -> Result<()> {
    let command = config.recovery_command.as_ref().unwrap_or(&config.command);
    deliver_with_status(text, config, command, "recovered").await
}

async fn deliver_with_status(
    text: &str,
    config: &OutputConfig,
    argv: &[String],
    status: &str,
) -> Result<()> {
    if config.clipboard {
        crate::platform::set_clipboard(text).context("clipboard delivery failed")?;
    }
    if argv.is_empty() {
        return Ok(());
    }
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .env("VIME_TRANSCRIPT_STATUS", status)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(windows_sys::Win32::System::Threading::CREATE_NO_WINDOW);
    let mut child = command.spawn().context("cannot start output command")?;
    let mut stdin = child
        .stdin
        .take()
        .context("output command stdin unavailable")?;
    let result = tokio::time::timeout(Duration::from_millis(config.script_timeout_ms), async {
        stdin
            .write_all(text.as_bytes())
            .await
            .context("cannot write transcript to command stdin")?;
        stdin
            .shutdown()
            .await
            .context("cannot close command stdin")?;
        drop(stdin);
        let status = child
            .wait()
            .await
            .context("cannot wait for output command")?;
        if !status.success() {
            bail!("output command exited with {status}");
        }
        Ok(())
    })
    .await;
    match result {
        Ok(result) => result,
        Err(_) => {
            child
                .kill()
                .await
                .context("cannot stop timed-out output command")?;
            let _ = child.wait().await;
            bail!(
                "output command timed out after {} ms",
                config.script_timeout_ms
            )
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    fn config(command: Vec<String>, timeout: u64) -> OutputConfig {
        OutputConfig {
            clipboard: false,
            command,
            recovery_command: None,
            script_timeout_ms: timeout,
        }
    }
    #[tokio::test]
    async fn stdin_preserves_literal_text() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("text");
        // A shell is explicitly supplied by this test, never implicitly by deliver.
        let c = config(
            vec![
                "sh".into(),
                "-c".into(),
                "cat > \"$1\"".into(),
                "test".into(),
                target.to_string_lossy().into(),
            ],
            1000,
        );
        let text = "中文\n$(touch unwanted) `literal` & %PATH%";
        deliver(text, &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(target).unwrap(), text);
    }
    #[tokio::test]
    async fn nonzero_and_timeout_are_errors() {
        assert!(deliver(
            "",
            &config(vec!["sh".into(), "-c".into(), "exit 7".into()], 1000)
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("exited"));
        assert!(deliver("", &config(vec!["sleep".into(), "10".into()], 20))
            .await
            .unwrap_err()
            .to_string()
            .contains("timed out"));
    }
    #[tokio::test]
    async fn recovery_script_selection_and_status() {
        let dir = tempfile::tempdir().unwrap();
        let regular = dir.path().join("regular");
        let recovered = dir.path().join("recovered");
        fn writer(path: &std::path::Path) -> Vec<String> {
            vec![
                "sh".into(),
                "-c".into(),
                "printf '%s\n' \"$VIME_TRANSCRIPT_STATUS\" > \"$1\"; cat >> \"$1\"".into(),
                "test".into(),
                path.to_string_lossy().into(),
            ]
        }
        let mut c = config(writer(&regular), 1000);
        deliver("full", &c).await.unwrap();
        assert_eq!(std::fs::read_to_string(&regular).unwrap(), "final\nfull");
        deliver_recovered("partial", &c).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&regular).unwrap(),
            "recovered\npartial"
        );
        c.recovery_command = Some(writer(&recovered));
        deliver_recovered("独立", &c).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&recovered).unwrap(),
            "recovered\n独立"
        );
        assert_eq!(
            std::fs::read_to_string(&regular).unwrap(),
            "recovered\npartial"
        );
        c.recovery_command = Some(vec![]);
        deliver_recovered("disabled", &c).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(&recovered).unwrap(),
            "recovered\n独立"
        );
    }
}
