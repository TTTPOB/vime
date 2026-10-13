#![cfg(windows)]

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};

const TONE_HZ: f64 = 440.0;
const PCM_RATE: f64 = 24_000.0;

#[test]
#[ignore = "requires reviewed VB-CABLE driver and working Windows Audio services"]
fn virtual_cable_wasapi_signal() {
    let host = cpal::host_from_id(cpal::HostId::Wasapi).unwrap();
    let input = host
        .input_devices()
        .unwrap()
        .find(|device| {
            device
                .name()
                .is_ok_and(|name| name.contains("CABLE Output"))
        })
        .expect(
            "UNSUPPORTED: no VB-CABLE recording endpoint; real WASAPI capture was not verified",
        );
    let output = host
        .output_devices()
        .unwrap()
        .find(|device| device.name().is_ok_and(|name| name.contains("CABLE Input")))
        .expect("UNSUPPORTED: no VB-CABLE playback endpoint; real WASAPI capture was not verified");
    let input_name = input.name().unwrap();
    let output_name = output.name().unwrap();
    let supported = output.default_output_config().unwrap();
    let config: cpal::StreamConfig = supported.clone().into();
    let channels = usize::from(config.channels);
    let source_rate = f64::from(config.sample_rate.0);
    let playback_failed = Arc::new(AtomicBool::new(false));
    println!("WASAPI endpoints: playback={output_name:?}, capture={input_name:?}, playback rate={source_rate}, channels={channels}");

    macro_rules! playback {
        ($sample:ty, $convert:expr) => {{
            let mut index: u64 = 0;
            let failed = playback_failed.clone();
            output
                .build_output_stream(
                    &config,
                    move |data: &mut [$sample], _| {
                        for frame in data.chunks_mut(channels) {
                            let signal = (std::f64::consts::TAU * TONE_HZ * index as f64
                                / source_rate)
                                .sin()
                                * 0.2;
                            frame.fill(($convert)(signal));
                            index += 1;
                        }
                    },
                    move |error| {
                        eprintln!("WASAPI playback error: {error}");
                        failed.store(true, Ordering::SeqCst);
                    },
                    None,
                )
                .unwrap()
        }};
    }
    let stream = match supported.sample_format() {
        cpal::SampleFormat::F32 => playback!(f32, |v: f64| v as f32),
        cpal::SampleFormat::I16 => playback!(i16, |v: f64| (v * 32767.0).round() as i16),
        cpal::SampleFormat::U16 => playback!(u16, |v: f64| ((v + 1.0) * 32767.5).round() as u16),
        format => panic!("UNSUPPORTED: playback sample format {format:?}"),
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let mut capture = vime::platform::capture::start(Some(&input_name)).unwrap();
    stream.play().unwrap();
    let mut pcm = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(8), async {
            let mut samples = Vec::new();
            while samples.len() < 48_000 {
                let frame = capture
                    .frames
                    .recv()
                    .await
                    .expect("capture channel closed before two seconds of PCM")
                    .expect("actual capture failed");
                samples.extend(frame);
            }
            samples
        })
        .await
        .expect("UNSUPPORTED: actual WASAPI capture did not yield two seconds of PCM")
    });
    stream.pause().unwrap();
    capture.stop().unwrap();
    assert!(
        !playback_failed.load(Ordering::SeqCst),
        "playback callback reported a device error"
    );
    assert!(
        (48_000..=72_000).contains(&pcm.len()),
        "unexpected 24k PCM duration: {} samples",
        pcm.len()
    );
    // Remove initial device startup silence, but require at least one full second
    // of actual signal. The 440 Hz projection checks content and normalization.
    let start = pcm
        .iter()
        .position(|&sample| sample.unsigned_abs() > 100)
        .expect("capture was silent");
    pcm = pcm.split_off(start);
    assert!(pcm.len() >= 24_000, "insufficient non-silent signal");
    let mut real = 0.0;
    let mut imaginary = 0.0;
    let mut energy = 0.0;
    for (index, &sample) in pcm.iter().enumerate() {
        let value = f64::from(sample);
        let phase = std::f64::consts::TAU * TONE_HZ * index as f64 / PCM_RATE;
        real += value * phase.cos();
        imaginary += value * phase.sin();
        energy += value * value;
    }
    let count = pcm.len() as f64;
    let rms = (energy / count).sqrt();
    let coherence = 2.0 * (real * real + imaginary * imaginary) / (count * energy);
    println!("captured PCM16 mono at 24kHz: {} signal samples, RMS={rms:.1}, 440Hz coherent energy={coherence:.4}", pcm.len());
    assert!(
        (1_000.0..=12_000.0).contains(&rms),
        "signal amplitude was unexpected"
    );
    assert!(
        coherence > 0.65,
        "expected 440Hz signal in normalized 24k PCM; actual coherent energy={coherence}"
    );
    live_client_e2e(&stream, &input_name);
}

struct TestChild(std::process::Child);
impl TestChild {
    fn assert_alive(&mut self, label: &str) {
        assert!(
            self.0.try_wait().unwrap().is_none(),
            "{label} exited prematurely"
        );
    }
    fn wait_success(&mut self, label: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        loop {
            if let Some(status) = self.0.try_wait().unwrap() {
                assert!(status.success(), "{label} exited with {status}");
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{label} did not terminate within three seconds"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for TestChild {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let deadline = std::time::Instant::now() + Duration::from_secs(3);
        while std::time::Instant::now() < deadline {
            match self.0.try_wait() {
                Ok(Some(_)) | Err(_) => return,
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            }
        }
        eprintln!("child did not finish reaping after termination within cleanup deadline");
    }
}

fn send_alt_f9(release: bool) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_F9, VK_MENU,
    };
    let keys = if release {
        [VK_F9, VK_MENU]
    } else {
        [VK_MENU, VK_F9]
    };
    let inputs = keys.map(|key| INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: key,
                wScan: 0,
                dwFlags: if release { KEYEVENTF_KEYUP } else { 0 },
                time: 0,
                dwExtraInfo: 0,
            },
        },
    });
    let inserted = unsafe { SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    assert_eq!(
        inserted,
        2,
        "live client SendInput failed: {}",
        std::io::Error::last_os_error()
    );
}
struct KeyRelease;
impl Drop for KeyRelease {
    fn drop(&mut self) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_F9, VK_MENU,
        };
        let inputs = [VK_F9, VK_MENU].map(|key| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: key,
                    wScan: 0,
                    dwFlags: KEYEVENTF_KEYUP,
                    time: 0,
                    dwExtraInfo: 0,
                },
            },
        });
        unsafe { SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    }
}

fn live_client_e2e(stream: &cpal::Stream, input_name: &str) {
    use std::{
        fs::File,
        os::windows::process::CommandExt,
        process::{Command, Stdio},
        time::Instant,
    };
    const DETACHED_PROCESS: u32 = 0x00000008;
    const FINAL: &str = "vime live 音频验证：你好。";
    let directory = tempfile::tempdir().unwrap();
    let ready = directory.path().join("mock.stdout");
    let report = directory.path().join("capture.json");
    let capture_wav = directory.path().join("capture.wav");
    let client_log = directory.path().join("client.log");
    let config_file = directory.path().join("vime.toml");
    let mut mock = TestChild(
        Command::new(env!("CARGO_BIN_EXE_vime-mock"))
            .args([
                "--bind",
                "127.0.0.1:0",
                "--text",
                FINAL,
                "--report",
                report.to_str().unwrap(),
                "--capture-wav",
                capture_wav.to_str().unwrap(),
            ])
            .stdin(Stdio::null())
            .stdout(File::create(&ready).unwrap())
            .stderr(File::create(directory.path().join("mock.stderr")).unwrap())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    let url = loop {
        mock.assert_alive("independent mock server");
        let stdout = std::fs::read_to_string(&ready).unwrap_or_default();
        if stdout.ends_with('\n') {
            if let Some(url) = stdout
                .lines()
                .find_map(|line| line.strip_prefix("LISTENING "))
            {
                break url.to_owned();
            }
        }
        assert!(
            Instant::now() < deadline,
            "mock readiness timed out; stdout={stdout}"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    let mut config = vime::config::Config::default();
    config.server.url = url;
    config.server.connect_timeout_ms = 2_000;
    config.server.final_timeout_ms = 2_000;
    std::fs::write(&config_file, toml::to_string(&config).unwrap()).unwrap();
    let mut client = TestChild(
        Command::new(env!("CARGO_BIN_EXE_vime"))
            .args([
                "--config",
                config_file.to_str().unwrap(),
                "--device",
                input_name,
                "--log-file",
                client_log.to_str().unwrap(),
            ])
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        client.assert_alive("detached live client");
        let logs = std::fs::read_to_string(&client_log).unwrap_or_default();
        if logs.contains("ready; configured hotkey starts recording") {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "live client readiness timed out: {logs}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    vime::platform::set_clipboard("vime live test sentinel").unwrap();
    stream.play().unwrap();
    let release = KeyRelease;
    send_alt_f9(false);
    std::thread::sleep(Duration::from_millis(700));
    send_alt_f9(true);
    drop(release);
    let mut clipboard = arboard::Clipboard::new().unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        client.assert_alive("live client awaiting clipboard final");
        mock.assert_alive("mock awaiting client upload");
        if clipboard.get_text().is_ok_and(|text| text == FINAL) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "no expected live final in clipboard; log={}",
            std::fs::read_to_string(&client_log).unwrap_or_default()
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    stream.pause().unwrap();
    // The independent server persists its report/WAV both at commit and at
    // socket teardown. Wait for a complete matching snapshot, never accept an
    // empty file observed midway through that second write.
    let deadline = Instant::now() + Duration::from_secs(2);
    let (captured, samples, spec) = loop {
        let snapshot = (|| -> Option<_> {
            let captured: vime::mock::CaptureReport =
                serde_json::from_slice(&std::fs::read(&report).ok()?).ok()?;
            let mut wav = hound::WavReader::open(&capture_wav).ok()?;
            let spec = wav.spec();
            let samples: Vec<i16> = wav.samples().collect::<Result<_, _>>().ok()?;
            (captured.sample_count > 0 && samples.len() == captured.sample_count)
                .then_some((captured, samples, spec))
        })();
        if let Some(snapshot) = snapshot {
            break snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "mock did not persist a complete matching PCM/report snapshot"
        );
        std::thread::sleep(Duration::from_millis(10));
    };
    assert!(captured.session_valid);
    assert_eq!(captured.sample_rate, 24_000);
    assert!(captured.append_count > 0);
    assert!(
        (2_400..=48_000).contains(&captured.sample_count),
        "unexpected live recording length: {}",
        captured.sample_count
    );
    assert_eq!(captured.commit_count, 1);
    assert_eq!(captured.clear_count, 0);
    assert!(
        captured.validation_error.is_none(),
        "mock session validation error: {:?}",
        captured.validation_error
    );
    assert_eq!(spec.sample_rate, 24_000);
    assert_eq!(spec.channels, 1);
    assert_eq!(spec.bits_per_sample, 16);
    assert_eq!(samples.len(), captured.sample_count);
    let mut real = 0.0;
    let mut imaginary = 0.0;
    let mut energy = 0.0;
    for (index, sample) in samples.iter().enumerate() {
        let value = f64::from(*sample);
        let phase = std::f64::consts::TAU * TONE_HZ * index as f64 / PCM_RATE;
        real += value * phase.cos();
        imaginary += value * phase.sin();
        energy += value * value;
    }
    let count = samples.len() as f64;
    let rms = (energy / count).sqrt();
    let coherence = 2.0 * (real * real + imaginary * imaginary) / (count * energy);
    println!("live client uploaded {} PCM samples in {} appends, one commit, clipboard={FINAL:?}; RMS={rms:.1}, 440Hz coherent energy={coherence:.4}", samples.len(), captured.append_count);
    assert!((1_000.0..=12_000.0).contains(&rms));
    assert!(
        coherence > 0.65,
        "uploaded live audio did not match actual cable tone"
    );
    let mut stop = TestChild(
        Command::new(env!("CARGO_BIN_EXE_vime"))
            .arg("--stop")
            .creation_flags(DETACHED_PROCESS)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    stop.wait_success("separate stop command");
    client.wait_success("live background client");
    drop(mock);
    println!("PASS: live SendInput -> actual WASAPI -> independent mock WebSocket -> final clipboard -> clean background shutdown");
}
