# Architecture

vime is a Windows user-session push-to-talk transcription client. Alt+F9
starts recording and release stops it; toggle mode is configurable. Each recording
uses an isolated WebSocket connection and delivers the acknowledged final text to
the clipboard and optional script. Interrupted recognition can recover received
text; cancellation and Restart discard pending recognition.

The main thread runs an Iced 0.14 daemon with the tiny-skia renderer. A worker
thread owns the current-thread Tokio runtime, recording, network, and delivery.
The native keyboard hook and notification-area icon each own their Windows message
loop. The UI receives the latest read-only `UiSnapshot` through a watch channel;
keyboard and tray actions enter one business control queue.

- `config.rs`: TOML server, hotkey, output, overlay and tray configuration.
- `protocol.rs`: documented Realtime transcription subset, PCM upload, commit and
  incoming delta/final events. Commit acknowledgement identifies the final item.
- `session.rs`: received text and authoritative final collection.
- `audio.rs`: WAV normalization and streaming mono 24kHz PCM resampling.
- `platform.rs`: keyboard hook, named shutdown event, single-instance mutex,
  WASAPI capture and clipboard.
- `output.rs`: clipboard and UTF-8 script stdin delivery, bounded execution and
  final/recovered status. Dropped script futures terminate their direct child.
- `overlay.rs`: on-demand read-only subtitles, system fonts, advanced shaping,
  bottom-anchored text, DPI-aware rendering and native nonactivating presentation.
- `tray.rs`: Show subtitles, Restart and Stop, Explorer restart restoration and
  native resource cleanup.
- `main.rs`: CLI diagnostics, desktop lifetime, state publication and recording.
- `mock.rs`: independent protocol mock; it returns preset text rather than ASR.

Restart cancels active recognition and recreates the keyboard listener using the
startup configuration. A TOML change applies after Stop and starting vime again.
The desktop process remains alive with zero visible windows; Stop or the named
`--stop` signal ends it. CLI diagnostics and WAV mode do not initialize the GUI.

Windows API probes and headless rendering have separate acceptance roles.
Screenshots validate rendering at specified scale factors; native desktop tests
validate focus and window lifetime. Neither proves physical microphone access or
cross-monitor DPI transitions. See `docs/OVERLAY.md` and `docs/TESTING.md`.
