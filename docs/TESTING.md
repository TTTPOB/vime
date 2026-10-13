# Validation boundaries

Run the portable checks with a stable Rust toolchain:

```sh
cargo fmt --all -- --check
cargo clippy --all-targets --locked -- -D warnings
cargo test --all-targets --locked
```

The `CI` workflow runs these checks on Linux and Windows Server 2022. The
`Windows connectivity probe` workflow can be dispatched separately before
implementation is complete. It only checks runner/toolchain availability and
prints the actual process session; it does not validate client functionality.

## Windows API evidence

`tests/windows_api.rs` is compiled only on Windows:

- `real_clipboard_unicode_roundtrip` calls the client's output API and reads
  the actual Windows clipboard back, including Chinese, a newline, and emoji.
- `register_hotkey_conflict_and_thread_message` registers a real hotkey, tests
  a conflicting registration on another thread, and explicitly posts a
  `WM_HOTKEY`. This tests API registration and message handling, **not** a
  keyboard-triggered hotkey.
- `single_instance_mutex_is_exclusive_and_released` checks real session mutex
  exclusivity and cleanup. `named_shutdown_event_reaches_listener_and_is_cleaned_up`
  signals the actual named shutdown event, receives the listener event, and
  verifies teardown releases the operating system object.
- `detached_background_client_stays_alive_and_stop_exits_cleanly` launches the
  actual GUI-subsystem executable with `DETACHED_PROCESS`, requires readiness
  within three seconds, proves it remains alive without a console, and starts
  a separate `--stop` process. Both processes must exit successfully within
  bounded deadlines; failure cleanup kills and reaps any surviving child.
- `send_input_probe` is ignored during ordinary tests and explicitly invoked
  in Windows CI. It injects Ctrl+Alt+Shift+F11 and requires an actual system
  `WM_HOTKEY` to arrive. An insertion failure or missing message fails the
  probe; it is never reported as successful by skipping the assertion.

```powershell
cargo test --locked --test windows_api send_input_probe -- --ignored --nocapture
```

These tests use and overwrite the user-session clipboard and inject synthetic
keyboard input. Use a disposable test desktop when running them locally.
`platform_send_input_probe` additionally drives the actual client listener
through push-to-talk and toggle modes, including key release and repeated
keydown suppression. Both input probes are explicitly invoked by the same
CI filter. Neither proves physical keyboard behavior or Windows 11 privacy
settings.

## Audio and backend evidence

The independent `vime-mock` process uses actual WebSocket connections. WAV
input tests validate audio normalization and network/output behavior, but do
not exercise a Windows recording device. WSS tests separately validate TLS.

Real WASAPI recording requires an available recording endpoint. A virtual
cable experiment must install a reviewed, signed driver and feed a known audio
signal through its playback endpoint into the client's actual capture code.
It must compare captured duration, sample rate, and signal content; creating
an output WAV or receiving canned mock text is insufficient evidence.
The core workflows do not install a virtual audio driver. The separately
dispatched `Optional real WASAPI probe` downloads a pinned official VB-CABLE
ZIP, checks its SHA-256 and trusted Windows signatures, and runs only the
original signed vendor installer. The disposable runner explicitly grants
microphone access through AppPrivacy policies and machine/user ConsentStore
fixtures after printing their previous non-secret values. These fixtures
model an authorized device and do not validate Windows 11 privacy UI behavior.
The workflow never imports certificates, bypasses
signing enforcement, or reboots the runner. Installation or endpoint failure
is reported as NOT VERIFIED. Its ignored `windows_audio` test sends a 440 Hz
tone through real WASAPI playback and the client capture code, then checks
24kHz normalized PCM duration, amplitude, and frequency coherence. A passing
run proves this virtual-device path, while physical microphone and privacy
checks remain separate.

## Recorded verification

On 2026-10-13, [core CI](https://github.com/TTTPOB/vime/actions/runs/38365349755)
passed on Linux and Windows: 33 portable tests on Linux; 34 ordinary Windows
tests plus both explicit `SendInput` probes. The Windows tests include detached
background startup and clean `--stop` shutdown.

The [signed virtual audio probe](https://github.com/TTTPOB/vime/actions/runs/38365344075)
also passed after explicit microphone permission fixtures: WASAPI playback was
48 kHz stereo, and the client's actual capture/normalization produced 24 kHz
mono PCM16 with 46,519 non-silent samples and the expected 440 Hz signal.
This proves the virtual WASAPI path, not physical microphone behavior, Windows
11 privacy UI, or real model recognition quality.

## Local Windows acceptance

On the target Windows machine, verify:

1. Alt+F9 press starts recording and release stops in push-to-talk mode;
   toggle mode starts/stops on separate presses without repeat triggers.
2. Chinese final text arrives in the clipboard, and an optional script gets
   exactly one final result through stdin. There is no automatic paste.
3. Actual microphone selection, privacy denial, silence, unplug/replug,
   and an unavailable device produce useful errors and allow recovery.
4. Delayed final responses, disconnects, cancellation, and subsequent
   recordings do not emit stale results or duplicate script invocations.
   On error or timeout, received text is recoverable and ordinary scripts run
   with `VIME_TRANSCRIPT_STATUS=recovered`; normal results use `final`.
   Cancellation emits no result. Explicit `recovery_command` overrides and
   disabling recovery scripts must behave as configured.
5. Real NAS WSS works with the documented event subset and a trusted TLS
   certificate; deploying that backend remains the user's separate task.

## Release artifacts

A pushed version tag such as `v0.1.0` triggers the `Release` workflow. It reruns
Windows formatting, lint, and tests, then packages both release executables,
the example TOML configuration, README, license, and protocol/testing docs.
The packaging script extracts the completed ZIP and runs its executable
with `--version` and the shipped example configuration with `--check-config`
before verifying its checksum. GitHub Release receives the ZIP and SHA-256
checksum. Only that job has
`contents: write`; ordinary CI has `contents: read`.

The release workflow runs ordinary Windows tests, while the explicit
`SendInput` capability probe belongs to CI. Check the CI run for the exact
release commit and disclose any capability that has not passed before calling
its Windows behavior validated.
