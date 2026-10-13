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
}
