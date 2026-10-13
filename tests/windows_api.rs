#![cfg(windows)]

use std::{
    ptr::null_mut,
    sync::Mutex,
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    System::Threading::GetCurrentThreadId,
    UI::{
        Input::KeyboardAndMouse::{
            RegisterHotKey, SendInput, UnregisterHotKey, INPUT, INPUT_0, INPUT_KEYBOARD,
            KEYBDINPUT, KEYEVENTF_KEYUP, MOD_ALT, MOD_CONTROL, MOD_NOREPEAT, MOD_SHIFT, VK_CONTROL,
            VK_F11, VK_MENU, VK_SHIFT,
        },
        WindowsAndMessaging::{PeekMessageW, PostThreadMessageW, MSG, PM_REMOVE, WM_HOTKEY},
    },
};

static DESKTOP_TEST: Mutex<()> = Mutex::new(());
const HOTKEY_ID: i32 = 0x5A11;
const MODIFIERS: u32 = MOD_CONTROL | MOD_ALT | MOD_SHIFT | MOD_NOREPEAT;

struct RegisteredHotkey;
impl RegisteredHotkey {
    fn new() -> Self {
        // Creating a thread message queue also makes PostThreadMessage deterministic.
        let mut message = MSG::default();
        unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) };
        assert_ne!(
            unsafe { RegisterHotKey(null_mut(), HOTKEY_ID, MODIFIERS, u32::from(VK_F11)) },
            0,
            "RegisterHotKey failed: {}",
            std::io::Error::last_os_error()
        );
        Self
    }
}
impl Drop for RegisteredHotkey {
    fn drop(&mut self) {
        unsafe { UnregisterHotKey(null_mut(), HOTKEY_ID) };
    }
}

fn wait_for_hotkey() -> Option<MSG> {
    let deadline = Instant::now() + Duration::from_secs(3);
    while Instant::now() < deadline {
        let mut message = MSG::default();
        if unsafe { PeekMessageW(&mut message, null_mut(), WM_HOTKEY, WM_HOTKEY, PM_REMOVE) } != 0
            && message.wParam == HOTKEY_ID as usize
        {
            return Some(message);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    None
}

#[test]
fn real_clipboard_unicode_roundtrip() {
    let _guard = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let expected = "vime 剪贴板测试：你好，世界！\nsecond line 🦀";
    vime::platform::set_clipboard(expected).unwrap();
    let mut clipboard = arboard::Clipboard::new().unwrap();
    assert_eq!(clipboard.get_text().unwrap(), expected);
}

#[test]
fn register_hotkey_conflict_and_thread_message() {
    let _guard = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _registered = RegisteredHotkey::new();
    std::thread::spawn(|| {
        let result = unsafe { RegisterHotKey(null_mut(), HOTKEY_ID, MODIFIERS, u32::from(VK_F11)) };
        if result != 0 {
            unsafe { UnregisterHotKey(null_mut(), HOTKEY_ID) };
        }
        assert_eq!(
            result, 0,
            "another thread must not register an occupied combination"
        );
    })
    .join()
    .unwrap();
    let payload = (((u32::from(VK_F11)) << 16) | (MODIFIERS & !MOD_NOREPEAT)) as isize;
    assert_ne!(
        unsafe { PostThreadMessageW(GetCurrentThreadId(), WM_HOTKEY, HOTKEY_ID as usize, payload) },
        0
    );
    let message = wait_for_hotkey().expect("posted WM_HOTKEY was not received");
    assert_eq!(message.lParam, payload);
    println!("PASS: API registration, conflict, and explicitly posted WM_HOTKEY; no keyboard injection claimed");
}

fn key_input(key: u16, release: bool) -> INPUT {
    INPUT {
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
    }
}

#[test]
#[ignore = "requires an interactive input desktop; CI invokes this capability probe explicitly"]
fn send_input_probe() {
    let _guard = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _registered = RegisteredHotkey::new();
    let keys = [VK_CONTROL, VK_MENU, VK_SHIFT, VK_F11];
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&key| key_input(key, false))
        .chain(keys.iter().rev().map(|&key| key_input(key, true)))
        .collect();
    let inserted = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    // Always attempt releases, even if Windows inserted only part of the batch.
    let releases: Vec<INPUT> = keys.iter().rev().map(|&key| key_input(key, true)).collect();
    unsafe {
        SendInput(
            releases.len() as u32,
            releases.as_ptr(),
            std::mem::size_of::<INPUT>() as i32,
        )
    };
    assert_eq!(
        inserted,
        inputs.len() as u32,
        "SendInput unavailable: {}",
        std::io::Error::last_os_error()
    );
    let message =
        wait_for_hotkey().expect("SendInput inserted keys but no system WM_HOTKEY arrived");
    assert_eq!((message.lParam as u32) >> 16, u32::from(VK_F11));
    println!("PASS: SendInput triggered an actual registered system WM_HOTKEY");
}

fn inject_keys(keys: &[(u16, bool)]) {
    let inputs: Vec<INPUT> = keys
        .iter()
        .map(|&(key, release)| key_input(key, release))
        .collect();
    assert_eq!(
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        },
        inputs.len() as u32
    );
}

struct ReleaseAltF9;
impl Drop for ReleaseAltF9 {
    fn drop(&mut self) {
        let inputs = [
            key_input(windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_F9, true),
            key_input(VK_MENU, true),
        ];
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        };
    }
}

#[test]
#[ignore = "requires an interactive input desktop; tests the actual client hotkey listener"]
fn platform_send_input_probe() {
    use vime::{
        config::{HotkeyConfig, HotkeyMode},
        ControlEvent,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::VK_F9;
    let _guard = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let _release = ReleaseAltF9;
    tokio::runtime::Runtime::new().unwrap().block_on(async {
        for mode in [HotkeyMode::PushToTalk, HotkeyMode::Toggle] {
            let toggle = matches!(mode, HotkeyMode::Toggle);
            let mut listener = vime::platform::hotkey::start(&HotkeyConfig {
                key: "Alt+F9".into(),
                mode,
            })
            .unwrap();
            inject_keys(&[(VK_MENU, false), (VK_F9, false)]);
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), listener.events.recv())
                    .await
                    .unwrap(),
                Some(ControlEvent::Start)
            );
            // A repeated keydown must not trigger another Start or toggle off.
            inject_keys(&[(VK_F9, false)]);
            assert!(
                tokio::time::timeout(Duration::from_millis(150), listener.events.recv())
                    .await
                    .is_err()
            );
            inject_keys(&[(VK_F9, true), (VK_MENU, true)]);
            if toggle {
                assert!(
                    tokio::time::timeout(Duration::from_millis(150), listener.events.recv())
                        .await
                        .is_err()
                );
                inject_keys(&[(VK_MENU, false), (VK_F9, false)]);
            }
            assert_eq!(
                tokio::time::timeout(Duration::from_secs(3), listener.events.recv())
                    .await
                    .unwrap(),
                Some(ControlEvent::Stop)
            );
            if toggle {
                inject_keys(&[(VK_F9, true), (VK_MENU, true)]);
            }
            drop(listener);
        }
    });
    println!("PASS: client hotkey listener handles push-to-talk, toggle, release, and repeated keydown via SendInput");
}

#[test]
fn single_instance_mutex_is_exclusive_and_released() {
    let _guard = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let instance = vime::platform::SingleInstance::acquire().unwrap();
    assert!(
        vime::platform::SingleInstance::acquire().is_err(),
        "a second instance in the same session must fail"
    );
    drop(instance);
    let replacement = vime::platform::SingleInstance::acquire()
        .expect("dropping the first instance must release its mutex");
    drop(replacement);
}

#[test]
fn named_shutdown_event_reaches_listener_and_is_cleaned_up() {
    use vime::{
        config::{HotkeyConfig, HotkeyMode},
        ControlEvent,
    };
    let _guard = DESKTOP_TEST
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    assert!(
        vime::platform::request_shutdown().is_err(),
        "no listener should own the named event yet"
    );
    let mut listener = vime::platform::hotkey::start(&HotkeyConfig {
        key: "Alt+F9".into(),
        mode: HotkeyMode::PushToTalk,
    })
    .unwrap();
    vime::platform::request_shutdown().unwrap();
    let event = tokio::runtime::Runtime::new().unwrap().block_on(async {
        tokio::time::timeout(Duration::from_secs(3), listener.events.recv())
            .await
            .unwrap()
    });
    assert_eq!(event, Some(ControlEvent::Shutdown));
    drop(listener);
    assert!(
        vime::platform::request_shutdown().is_err(),
        "dropping the listener must release its named event"
    );
}
