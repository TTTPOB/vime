# Agreed scope and module contract

Rust no-GUI Windows user-session background client. Default hotkey Alt+F9,
push-to-talk (press starts, release stops), configurable toggle mode too.
Final text clipboard and optional script stdin; errors recover received text,
defaulting to the ordinary script, optionally a separate recovery_command.
Scripts receive VIME_TRANSCRIPT_STATUS=final/recovered. Cancellation never delivers.
No auto-paste, ASR model, LLM,
history, tray, UI, database. Independent WebSocket mock. Self-hosted NAS later.
Protocol is a documented Realtime-inspired subset, not full OpenAI compatibility.

Architecture:
- config.rs: Config { server: ServerConfig, hotkey: HotkeyConfig, output: OutputConfig };
  ServerConfig { url:String, model:String, api_key_env:Option<String>, connect_timeout_ms:u64,
  final_timeout_ms:u64 }; HotkeyConfig { key:String, mode:HotkeyMode };
  HotkeyMode { PushToTalk, Toggle }; OutputConfig { clipboard:bool,
  command:Vec<String>, recovery_command:Option<Vec<String>>, script_timeout_ms:u64 }; Config::load(path), validate().
- protocol.rs: Connection::connect(&ServerConfig) async -> Result<Self>;
  append(&[i16]), commit(), cancel() async; next_event() async -> Result<ServerEvent>.
  ServerEvent { Delta {item_id:String,text:String}, Committed {item_id:String},
  Final {item_id:String,text:String}, Error {message:String}, Other }.
  24kHz mono little-endian PCM16 base64 JSON append, manual commit with VAD off.
  Each recording is isolated in its own connection; commit acknowledgement identifies final item.
- audio.rs: load_wav(path)->Result<Vec<i16>> (normalize to 24kHz mono), resampler helpers.
- platform.rs: platform::hotkey::start(&HotkeyConfig)->Result<HotkeyListener>,
  HotkeyListener { pub events:tokio::sync::mpsc::UnboundedReceiver<ControlEvent> }, Drop cleanup.
  platform::capture::start(device:Option<&str>)->Result<Capture>, Capture { pub frames:
  tokio::sync::mpsc::Receiver<Result<Vec<i16>,String>> }; Capture::stop(&mut self)->Result<()>.
  platform::SingleInstance::acquire()->Result<Self>.
  platform::set_clipboard(text:&str)->Result<()>.
- output.rs: deliver(text:&str,config:&OutputConfig) async -> Result<()>;
  stdin only, bounded script execution, nonzero status errors, clipboard optional for CI.
- mock.rs: reusable independent network mock and CLI vime-mock;
  success/duplicate/error/disconnect/timeout/delayed/cancel scenario support.
- main.rs (root): integrates live mode, config check, connection test,
  WAV command-line mode for CI, timeout/cancel/error recovery, logging.

No agents commit/push independently. Each owns assigned files only. Coordinate
API changes with orchestrator. Tests must prove behavior, never replace Windows
validation with a claim based on source inspection. Release on tag via Actions.
