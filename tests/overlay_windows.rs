#![cfg(windows)]
use std::{
    process::{Child, Command},
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

struct Demo(Child);
impl Drop for Demo {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}
fn overlay_window() -> windows_sys::Win32::Foundation::HWND {
    let title: Vec<u16> = "vime subtitles".encode_utf16().chain(Some(0)).collect();
    unsafe { FindWindowW(null_mut(), title.as_ptr()) }
}

#[test]
#[ignore = "requires an interactive Windows desktop"]
fn overlay_preserves_focus_and_returns_to_background() {
    let foreground = unsafe { GetForegroundWindow() };
    assert!(
        !foreground.is_null(),
        "interactive desktop must have a foreground window"
    );
    let mut demo = Demo(
        Command::new(env!("CARGO_BIN_EXE_vime"))
            .args(["--overlay-demo", "--no-tray"])
            .spawn()
            .expect("start demo"),
    );
    let deadline = Instant::now() + Duration::from_secs(6);
    let window = loop {
        let window = overlay_window();
        if !window.is_null() && unsafe { IsWindowVisible(window) } != 0 {
            break window;
        }
        assert!(
            demo.0.try_wait().unwrap().is_none(),
            "demo exited before showing its window"
        );
        assert!(Instant::now() < deadline, "overlay was not shown");
        std::thread::sleep(Duration::from_millis(25));
    };
    let style = unsafe { GetWindowLongPtrW(window, GWL_EXSTYLE) } as u32;
    assert_ne!(style & WS_EX_NOACTIVATE, 0, "overlay must not activate");
    assert_ne!(
        style & WS_EX_TOOLWINDOW,
        0,
        "tool window must be excluded from taskbar"
    );
    assert_eq!(
        style & WS_EX_APPWINDOW,
        0,
        "overlay must not force a taskbar entry"
    );
    assert_ne!(
        style & WS_EX_TOPMOST,
        0,
        "overlay must remain above ordinary windows"
    );
    assert_eq!(
        unsafe { GetForegroundWindow() },
        foreground,
        "opening overlay stole focus"
    );
    let deadline = Instant::now() + Duration::from_secs(12);
    while !overlay_window().is_null() {
        assert!(
            Instant::now() < deadline,
            "overlay did not return to background"
        );
        assert_eq!(
            unsafe { GetForegroundWindow() },
            foreground,
            "overlay stole focus during updates"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        demo.0.try_wait().unwrap().is_none(),
        "closing last window terminated daemon"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop"]
fn no_window_demo_never_creates_overlay() {
    let mut demo = Demo(
        Command::new(env!("CARGO_BIN_EXE_vime"))
            .args(["--overlay-demo", "--no-window", "--no-tray"])
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(8);
    while Instant::now() < deadline {
        assert!(overlay_window().is_null(), "--no-window created an overlay");
        assert!(demo.0.try_wait().unwrap().is_none(), "demo exited early");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn tray_window() -> windows_sys::Win32::Foundation::HWND {
    let class: Vec<u16> = "VimeTrayWindow".encode_utf16().chain(Some(0)).collect();
    unsafe { FindWindowW(class.as_ptr(), null_mut()) }
}

fn choose_tray_item(window: windows_sys::Win32::Foundation::HWND, down: usize) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_DOWN, VK_HOME,
        VK_RETURN,
    };
    assert_ne!(
        unsafe { PostMessageW(window, WM_APP + 1, 0, WM_RBUTTONUP as isize) },
        0
    );
    let popup_class: Vec<u16> = "#32768".encode_utf16().chain(Some(0)).collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let popup = unsafe { FindWindowW(popup_class.as_ptr(), null_mut()) };
        if !popup.is_null() && unsafe { IsWindowVisible(popup) } != 0 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tray popup did not open; foreground={:?}",
            unsafe { GetForegroundWindow() }
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    let keys = std::iter::once(VK_HOME)
        .chain(std::iter::repeat_n(VK_DOWN, down))
        .chain(std::iter::once(VK_RETURN));
    let mut inputs = Vec::new();
    for key in keys {
        for flags in [0, KEYEVENTF_KEYUP] {
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: key,
                        dwFlags: flags,
                        ..Default::default()
                    },
                },
            });
        }
    }
    assert_eq!(
        unsafe {
            SendInput(
                inputs.len() as u32,
                inputs.as_ptr(),
                std::mem::size_of::<INPUT>() as i32,
            )
        },
        inputs.len() as u32,
        "could not navigate actual tray menu"
    );
}

#[test]
#[ignore = "requires an interactive Windows desktop and Explorer notification area"]
fn actual_tray_menu_restarts_listener_and_stops_process() {
    let directory = tempfile::tempdir().unwrap();
    let config = directory.path().join("vime.toml");
    let log = directory.path().join("vime.log");
    std::fs::write(
        &config,
        "[hotkey]\nkey = \"Ctrl+Alt+Shift+F10\"\n[output]\nclipboard = false\n",
    )
    .unwrap();
    let mut client = Demo(
        Command::new(env!("CARGO_BIN_EXE_vime"))
            .args(["--no-window", "--config"])
            .arg(&config)
            .arg("--log-file")
            .arg(&log)
            .spawn()
            .expect("start actual tray client"),
    );
    unsafe {
        AllowSetForegroundWindow(client.0.id());
    }
    let wait_ready = |client: &mut Demo, expected: usize| {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let text = std::fs::read_to_string(&log).unwrap_or_default();
            if text
                .matches("ready; configured hotkey starts recording")
                .count()
                >= expected
            {
                return;
            }
            assert!(
                client.0.try_wait().unwrap().is_none(),
                "tray client exited before listener became ready: {text}"
            );
            assert!(
                Instant::now() < deadline,
                "listener did not become ready {expected} times: {text}"
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    };
    wait_ready(&mut client, 1);
    let window = tray_window();
    assert!(!window.is_null(), "native tray window was not created");
    let mut owner = 0;
    unsafe {
        GetWindowThreadProcessId(window, &mut owner);
    }
    assert_eq!(
        owner,
        client.0.id(),
        "tray window belongs to another process"
    );
    choose_tray_item(window, 1);
    wait_ready(&mut client, 2);
    assert!(
        overlay_window().is_null(),
        "windowless restart created subtitle window"
    );
    choose_tray_item(window, 2);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(status) = client.0.try_wait().unwrap() {
            assert!(status.success(), "tray Stop failed: {status}");
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tray Stop did not exit client; foreground={:?}",
            unsafe { GetForegroundWindow() }
        );
        std::thread::sleep(Duration::from_millis(25));
    }
    assert!(
        tray_window().is_null(),
        "tray native window leaked after Stop"
    );
}
