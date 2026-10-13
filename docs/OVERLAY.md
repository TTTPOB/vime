# Subtitle overlay and tray

The Windows desktop client runs in the background. A compact subtitle card appears while recording and follows the newest text. Success hides after 0.6 seconds; recovery and error notices remain for 2 seconds. Processing is shown in red, and the hotkey refuses new recordings until final transcription and delivery finish. Busy-time key presses are discarded. Closing the card leaves the client running. The tray provides restart, an overlay toggle and stop. Restart cancels a pending recording and recreates the transcription worker and hotkey with the startup configuration. Stop cancels pending work and exits the program. Restart the program to load configuration changes.

Configure the overlay in `vime.toml`:

```toml
[overlay]
enabled = true
font_family = "Microsoft YaHei UI"
```

The font must be installed on the machine. An unavailable family produces a warning and uses system sans-serif. Iced's advanced shaping handles mixed Chinese/English text and system fallback; vime does not bundle fonts. `--no-window` runs without the subtitle card; `--no-tray` disables the tray. Combining them runs only the keyboard-driven background engine without initializing the GUI.

## Review the interface

Run `cargo test --test overlay_render`. Linux needs `fonts-noto-cjk`; Windows needs the Simplified Chinese supplemental fonts, including Microsoft YaHei UI. The test checks the installed font's character map for the mixed-language samples and generates 32 PNGs under `target/overlay-screenshots/`. Open `index.html` in that directory to review connecting, recording, finishing, delivered, recovered and error states, plus three-line and long transcripts, at 100%, 125%, 150% and 200% scale. Screenshots use the official `iced_test::screenshot` API and the production view.

CI uploads the screenshots and HTML as `overlay-review-Linux` and `overlay-review-Windows`. Download and extract the artifact, then open `index.html`. Check glyphs, line wrapping, the newest long-transcript line and footer truncation. Screenshot generation verifies rendering and physical dimensions; visual quality still needs review.

Run the deterministic UI preview without a microphone or server:

```powershell
vime.exe --overlay-demo --no-tray
```

On an interactive Windows desktop, run the lifecycle checks with:

```powershell
cargo test --test overlay_windows -- --ignored --nocapture --test-threads=1
```

These launch the real demo, verify that its window keeps the foreground application focused, sets nonactivating/tool-window/topmost styles, disappears while the process remains alive, and respects `--no-window`. They run explicitly in Windows CI. Desktop-session restrictions can make these checks fail on runners without an interactive foreground window.

## Resource measurements

After `cargo build --release --bins`, run `./scripts/overlay-metrics.ps1`. The JSON report records executable size, sampled working set and CPU for the visible demo and windowless demo. CPU is expressed as a percentage of one core. CI includes the report in `vime-windows-x64`. These measurements isolate the desktop shell: microphone capture and network transcription are absent from the demo, and runner measurements are not a substitute for measurements on the target desktop.
