//! Windows user-session integration. Live operations deliberately fail on other OSes.
use anyhow::{bail, Result};

#[cfg(not(windows))]
pub struct SingleInstance;
#[cfg(not(windows))]
pub fn attach_parent_console() {}
#[cfg(not(windows))]
pub fn request_shutdown() -> Result<()> {
    bail!("background shutdown requires Windows")
}
#[cfg(not(windows))]
impl SingleInstance {
    pub fn acquire() -> Result<Self> {
        bail!("live mode requires a Windows user session")
    }
}
#[cfg(not(windows))]
pub fn set_clipboard(_: &str) -> Result<()> {
    bail!("clipboard output requires Windows")
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::{
        Foundation::{CloseHandle, GetLastError, ERROR_ALREADY_EXISTS, HANDLE},
        System::Threading::{CreateMutexW, OpenEventW, SetEvent, EVENT_MODIFY_STATE},
    };
    pub fn attach_parent_console() {
        unsafe {
            windows_sys::Win32::System::Console::AttachConsole(
                windows_sys::Win32::System::Console::ATTACH_PARENT_PROCESS,
            );
        }
    }
    pub struct SingleInstance(HANDLE);
    impl SingleInstance {
        pub fn acquire() -> Result<Self> {
            let name: Vec<u16> = "Local\\vime-client-v1\0".encode_utf16().collect();
            let handle = unsafe { CreateMutexW(std::ptr::null(), 0, name.as_ptr()) };
            if handle.is_null() {
                return Err(std::io::Error::last_os_error().into());
            }
            if unsafe { GetLastError() } == ERROR_ALREADY_EXISTS {
                unsafe {
                    CloseHandle(handle);
                }
                bail!("vime is already running in this Windows session");
            }
            Ok(Self(handle))
        }
    }
    impl Drop for SingleInstance {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }
    pub fn request_shutdown() -> Result<()> {
        let name: Vec<u16> = "Local\\vime-shutdown-v1\0".encode_utf16().collect();
        let event = unsafe { OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr()) };
        if event.is_null() {
            bail!(
                "no running vime shutdown event: {}",
                std::io::Error::last_os_error()
            );
        }
        let result = unsafe { SetEvent(event) };
        unsafe {
            CloseHandle(event);
        }
        if result == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(())
    }
    pub fn set_clipboard(text: &str) -> Result<()> {
        arboard::Clipboard::new()?.set_text(text.to_owned())?;
        Ok(())
    }
}
#[cfg(windows)]
pub use win::{attach_parent_console, request_shutdown, set_clipboard, SingleInstance};

pub mod hotkey {
    use super::*;
    use crate::{config::HotkeyConfig, ControlEvent};
    use tokio::sync::mpsc;
    pub struct HotkeyListener {
        pub events: mpsc::UnboundedReceiver<ControlEvent>,
        #[cfg(windows)]
        thread: Option<std::thread::JoinHandle<()>>,
        #[cfg(windows)]
        thread_id: u32,
        #[cfg(windows)]
        resets: std::sync::mpsc::Sender<std::sync::mpsc::SyncSender<()>>,
    }
    impl HotkeyListener {
        pub fn reset(&self) -> Result<()> {
            #[cfg(windows)]
            {
                let (tx, rx) = std::sync::mpsc::sync_channel(1);
                self.resets.send(tx)?;
                if unsafe {
                    windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                        self.thread_id,
                        windows_sys::Win32::UI::WindowsAndMessaging::WM_APP,
                        0,
                        0,
                    )
                } == 0
                {
                    return Err(std::io::Error::last_os_error().into());
                }
                rx.recv_timeout(std::time::Duration::from_secs(1))?;
            }
            Ok(())
        }
    }
    #[cfg(not(windows))]
    pub fn start(_: &HotkeyConfig) -> Result<HotkeyListener> {
        bail!("global hotkeys require Windows")
    }
    #[cfg(windows)]
    pub fn start(config: &HotkeyConfig) -> Result<HotkeyListener> {
        windows::start(config)
    }
    #[cfg(windows)]
    impl Drop for HotkeyListener {
        fn drop(&mut self) {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostThreadMessageW(
                    self.thread_id,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_QUIT,
                    0,
                    0,
                );
            }
            if let Some(thread) = self.thread.take() {
                let _ = thread.join();
            }
        }
    }
    pub fn validate_key(key: &str) -> Result<()> {
        parse_key(key).map(|_| ())
    }
    fn parse_key(key: &str) -> Result<(u32, Vec<u32>)> {
        let mut modifiers = Vec::new();
        let mut primary = None;
        for part in key.split('+').map(|s| s.trim().to_ascii_uppercase()) {
            let modifier = match part.as_str() {
                "ALT" => Some(0x12),
                "CTRL" | "CONTROL" => Some(0x11),
                "SHIFT" => Some(0x10),
                "WIN" | "SUPER" => Some(0x5b),
                _ => None,
            };
            if let Some(m) = modifier {
                if !modifiers.contains(&m) {
                    modifiers.push(m);
                }
                continue;
            }
            let value = if part.starts_with('F') && part.len() > 1 {
                let n: u32 = part[1..]
                    .parse()
                    .map_err(|_| anyhow::anyhow!("invalid hotkey {key}"))?;
                if !(1..=24).contains(&n) {
                    bail!("function key must be F1 through F24");
                }
                0x70 + n - 1
            } else if part.len() == 1 && part.as_bytes()[0].is_ascii_alphanumeric() {
                part.as_bytes()[0] as u32
            } else {
                match part.as_str() {
                    "SPACE" => 0x20,
                    "TAB" => 0x09,
                    "ENTER" => 0x0d,
                    "ESC" | "ESCAPE" => 0x1b,
                    _ => bail!("unsupported hotkey key {part}"),
                }
            };
            if primary.replace(value).is_some() {
                bail!("hotkey must have one primary key");
            }
        }
        Ok((
            primary.ok_or_else(|| anyhow::anyhow!("hotkey needs a primary key"))?,
            modifiers,
        ))
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn parses_common_hotkeys_and_rejects_invalid_keys() {
            for key in ["Alt+F9", "ctrl+shift+F24", "Win+Space", "Control+A", "F1"] {
                validate_key(key).unwrap();
            }
            for key in [
                "",
                "Alt",
                "Alt+F99",
                "Alt+F0",
                "Alt+Ffoo",
                "Ctrl+A+B",
                "Alt++F9",
                "Ctrl+Home",
            ] {
                assert!(validate_key(key).is_err(), "accepted {key}");
            }
            assert_eq!(parse_key("Alt+F9").unwrap(), (0x78, vec![0x12]));
        }
    }
    #[cfg(windows)]
    mod windows {
        use super::*;
        use crate::config::HotkeyMode;
        use std::{cell::RefCell, sync::mpsc as sync};
        use windows_sys::Win32::{
            Foundation::*,
            System::{
                LibraryLoader::GetModuleHandleW,
                Threading::{CreateEventW, GetCurrentThreadId, WaitForSingleObject},
            },
            UI::{Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
        };
        struct State {
            key: u32,
            modifiers: Vec<u32>,
            mode: HotkeyMode,
            held: bool,
            active: bool,
            sender: mpsc::UnboundedSender<ControlEvent>,
        }
        thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }
        fn is_modifier(key: u32, expected: u32) -> bool {
            key == expected
                || match expected {
                    0x12 => key == 0xa4 || key == 0xa5,
                    0x11 => key == 0xa2 || key == 0xa3,
                    0x10 => key == 0xa0 || key == 0xa1,
                    0x5b => key == 0x5c,
                    _ => false,
                }
        }
        unsafe extern "system" fn callback(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
            if code >= 0 {
                let key = (*(lparam as *const KBDLLHOOKSTRUCT)).vkCode;
                let down = wparam as u32 == WM_KEYDOWN || wparam as u32 == WM_SYSKEYDOWN;
                let up = wparam as u32 == WM_KEYUP || wparam as u32 == WM_SYSKEYUP;
                STATE.with(|cell| {
                    if let Some(s) = cell.borrow_mut().as_mut() {
                        if up && key == s.key {
                            s.held = false;
                        }
                        let release = up
                            && (key == s.key || s.modifiers.iter().any(|&m| is_modifier(key, m)));
                        if release && s.active && matches!(s.mode, HotkeyMode::PushToTalk) {
                            s.active = false;
                            let _ = s.sender.send(ControlEvent::Stop);
                        }
                        if down && key == 0x1b {
                            s.active = false;
                            let _ = s.sender.send(ControlEvent::Cancel);
                        }
                        if down && key == s.key && !s.held {
                            s.held = true;
                            if s.modifiers.iter().all(|&m| GetAsyncKeyState(m as i32) < 0) {
                                if matches!(s.mode, HotkeyMode::Toggle) && s.active {
                                    s.active = false;
                                    let _ = s.sender.send(ControlEvent::Stop);
                                } else if !s.active {
                                    s.active = true;
                                    let _ = s.sender.send(ControlEvent::Start);
                                }
                            }
                        }
                    }
                });
            }
            CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
        }
        pub fn start(config: &HotkeyConfig) -> Result<HotkeyListener> {
            let (key, modifiers) = parse_key(&config.key)?;
            let mode = config.mode.clone();
            let (sender, events) = mpsc::unbounded_channel();
            let (ready_tx, ready_rx) = sync::sync_channel(1);
            let (reset_tx, reset_rx) = sync::channel::<sync::SyncSender<()>>();
            let thread = std::thread::spawn(move || unsafe {
                // Force creation of the message queue before publishing the thread id.
                let mut message: MSG = std::mem::zeroed();
                PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_NOREMOVE);
                STATE.with(|s| {
                    *s.borrow_mut() = Some(State {
                        key,
                        modifiers,
                        mode,
                        held: false,
                        active: false,
                        sender,
                    })
                });
                let hook = SetWindowsHookExW(
                    WH_KEYBOARD_LL,
                    Some(callback),
                    GetModuleHandleW(std::ptr::null()),
                    0,
                );
                if hook.is_null() {
                    let _ = ready_tx.send(Err(std::io::Error::last_os_error().to_string()));
                    return;
                }
                let name: Vec<u16> = "Local\\vime-shutdown-v1\0".encode_utf16().collect();
                let shutdown = CreateEventW(std::ptr::null(), 0, 0, name.as_ptr());
                if shutdown.is_null() {
                    UnhookWindowsHookEx(hook);
                    let _ = ready_tx.send(Err(std::io::Error::last_os_error().to_string()));
                    return;
                }
                let timer = SetTimer(std::ptr::null_mut(), 0, 100, None);
                if timer == 0 {
                    CloseHandle(shutdown);
                    UnhookWindowsHookEx(hook);
                    let _ = ready_tx.send(Err(std::io::Error::last_os_error().to_string()));
                    return;
                }
                let _ = ready_tx.send(Ok(GetCurrentThreadId()));
                while GetMessageW(&mut message, std::ptr::null_mut(), 0, 0) > 0 {
                    if message.message == WM_APP {
                        for ack in reset_rx.try_iter() {
                            STATE.with(|s| {
                                if let Some(state) = s.borrow_mut().as_mut() {
                                    state.active = false;
                                }
                            });
                            let _ = ack.send(());
                        }
                    }
                    if message.message == WM_TIMER
                        && WaitForSingleObject(shutdown, 0) == WAIT_OBJECT_0
                    {
                        STATE.with(|s| {
                            if let Some(state) = s.borrow().as_ref() {
                                let _ = state.sender.send(ControlEvent::Shutdown);
                            }
                        });
                    }
                    TranslateMessage(&message);
                    DispatchMessageW(&message);
                }
                KillTimer(std::ptr::null_mut(), timer);
                CloseHandle(shutdown);
                UnhookWindowsHookEx(hook);
                STATE.with(|s| *s.borrow_mut() = None);
            });
            match ready_rx.recv()? {
                Ok(thread_id) => Ok(HotkeyListener {
                    events,
                    thread: Some(thread),
                    thread_id,
                    resets: reset_tx,
                }),
                Err(error) => {
                    let _ = thread.join();
                    bail!("install keyboard hook: {error}");
                }
            }
        }
    }
}

pub mod capture {
    use super::*;
    use tokio::sync::mpsc;
    pub struct Capture {
        pub frames: mpsc::Receiver<std::result::Result<Vec<i16>, String>>,
        #[cfg(windows)]
        stream: Option<cpal::Stream>,
        #[cfg(windows)]
        resampler: std::sync::Arc<std::sync::Mutex<crate::audio::Resampler>>,
        #[cfg(windows)]
        sender: Option<mpsc::Sender<std::result::Result<Vec<i16>, String>>>,
        #[cfg(windows)]
        failed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    }
    #[cfg(not(windows))]
    pub fn start(_: Option<&str>) -> Result<Capture> {
        bail!("microphone capture requires Windows")
    }
    impl Capture {
        pub fn stop(&mut self) -> Result<()> {
            #[cfg(not(windows))]
            {
                bail!("microphone capture requires Windows")
            }
            #[cfg(windows)]
            {
                use cpal::traits::StreamTrait;
                if self.stream.is_none() {
                    return Ok(());
                }
                if let Some(stream) = self.stream.take() {
                    let pause = stream.pause();
                    drop(stream);
                    if let Err(e) = pause {
                        self.sender.take();
                        return Err(e.into());
                    }
                }
                if self.failed.load(std::sync::atomic::Ordering::SeqCst) {
                    self.sender.take();
                    bail!("microphone capture failed (device error or audio queue overflow)");
                }
                let tail = self
                    .resampler
                    .lock()
                    .map_err(|_| anyhow::anyhow!("audio state poisoned"))?
                    .finish();
                let sender = self.sender.take();
                if !tail.is_empty() {
                    if let Some(sender) = sender {
                        sender
                            .try_send(Ok(tail))
                            .map_err(|_| anyhow::anyhow!("audio queue overflow while stopping"))?;
                    }
                }
                Ok(())
            }
        }
    }
    #[cfg(windows)]
    pub fn start(device_name: Option<&str>) -> Result<Capture> {
        use cpal::{
            traits::{DeviceTrait, HostTrait, StreamTrait},
            SampleFormat,
        };
        use std::sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        };
        let host = cpal::host_from_id(cpal::HostId::Wasapi)?;
        let device = match device_name {
            Some(name) => host
                .input_devices()?
                .find(|d| d.name().ok().as_deref() == Some(name))
                .ok_or_else(|| anyhow::anyhow!("input device not found: {name}"))?,
            None => host
                .default_input_device()
                .ok_or_else(|| anyhow::anyhow!("no default microphone"))?,
        };
        let supported = device.default_input_config()?;
        let channels = supported.channels() as usize;
        let config: cpal::StreamConfig = supported.clone().into();
        let resampler = Arc::new(Mutex::new(crate::audio::Resampler::new(
            config.sample_rate.0,
        )?));
        let failed = Arc::new(AtomicBool::new(false));
        let (sender, frames) = mpsc::channel(512);
        let report = {
            let sender = sender.clone();
            let failed = failed.clone();
            move |error: String| {
                if !failed.swap(true, Ordering::SeqCst) {
                    if let Err(mpsc::error::TrySendError::Full(error)) = sender.try_send(Err(error))
                    {
                        let sender = sender.clone();
                        std::thread::spawn(move || {
                            let _ = sender.blocking_send(error);
                        });
                    }
                }
            }
        };
        macro_rules! build {
            ($ty:ty, $convert:expr) => {{
                let resampler = resampler.clone();
                let sender = sender.clone();
                let failed = failed.clone();
                let report_audio = report.clone();
                let report_error = report.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[$ty], _| {
                        if failed.load(Ordering::SeqCst) {
                            return;
                        }
                        let samples: Vec<f32> = data.iter().map($convert).collect();
                        let mono = match crate::audio::downmix(&samples, channels) {
                            Ok(v) => v,
                            Err(e) => {
                                report_audio(e.to_string());
                                return;
                            }
                        };
                        let pcm = match resampler.lock() {
                            Ok(mut r) => r.push(&mono),
                            Err(_) => {
                                report_audio("audio state poisoned".into());
                                return;
                            }
                        };
                        if pcm.is_empty() {
                            return;
                        }
                        // Reserve one slot for an explicit terminal error; never quietly lose PCM.
                        for frame in pcm.chunks(480) {
                            if sender.capacity() <= 1 {
                                report_audio("audio queue overflow; recording aborted".into());
                                return;
                            }
                            if sender.try_send(Ok(frame.to_vec())).is_err() {
                                report_audio("audio queue closed; recording aborted".into());
                                return;
                            }
                        }
                    },
                    move |error| report_error(format!("microphone error: {error}")),
                    None,
                )?
            }};
        }
        let stream = match supported.sample_format() {
            SampleFormat::F32 => build!(f32, |v: &f32| *v),
            SampleFormat::I16 => build!(i16, |v: &i16| *v as f32 / 32768.0),
            SampleFormat::U16 => build!(u16, |v: &u16| (*v as f32 - 32768.0) / 32768.0),
            SampleFormat::I32 => build!(i32, |v: &i32| (*v as f64 / 2147483648.0) as f32),
            SampleFormat::F64 => build!(f64, |v: &f64| *v as f32),
            other => bail!("unsupported microphone sample format: {other:?}"),
        };
        stream.play()?;
        Ok(Capture {
            frames,
            stream: Some(stream),
            resampler,
            sender: Some(sender),
            failed,
        })
    }
}
