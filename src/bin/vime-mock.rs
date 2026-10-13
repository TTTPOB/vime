use anyhow::Result;
use clap::Parser;
use std::{io::Write, net::SocketAddr, path::PathBuf};
use tokio::net::TcpListener;
use vime::mock::{serve, MockOptions, Scenario};

/// Deterministic speech-protocol mock; does not perform speech recognition.
#[derive(Parser)]
#[command(version)]
struct Args {
    #[arg(long, default_value = "127.0.0.1:8765")]
    bind: SocketAddr,
    #[arg(long, value_enum, default_value = "success")]
    scenario: Scenario,
    #[arg(long, default_value = "你好，世界。Hello, world!")]
    text: String,
    #[arg(long, default_value_t = 250)]
    delay_ms: u64,
    /// Write counters and protocol validation results as JSON.
    #[arg(long)]
    report: Option<PathBuf>,
    /// Write received audio as mono PCM16 24 kHz WAV.
    #[arg(long)]
    capture_wav: Option<PathBuf>,
}
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let options = MockOptions {
        bind: args.bind,
        scenario: args.scenario,
        text: args.text,
        delay_ms: args.delay_ms,
        report: args.report,
        capture_wav: args.capture_wav,
    };
    let listener = TcpListener::bind(options.bind).await?;
    println!("LISTENING ws://{}", listener.local_addr()?);
    std::io::stdout().flush()?;
    tokio::select! {
        result = serve(listener, options) => result,
        result = tokio::signal::ctrl_c() => { result?; Ok(()) },
    }
}
