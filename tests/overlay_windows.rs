#![cfg(windows)]
use std::{
    process::{Child, Command},
    ptr::null_mut,
    time::{Duration, Instant},
};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// A real visible foreground application makes nonactivation assertions and
// synthetic tray gestures independent of the runner console's focus ownership.
struct ForegroundTarget(windows_sys::Win32::Foundation::HWND);
impl ForegroundTarget {
    fn new() -> Self {
        let class: Vec<u16> = "STATIC".encode_utf16().chain(Some(0)).collect();
        let title: Vec<u16> = "vime desktop test target"
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let window = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                80,
                80,
                320,
                200,
                null_mut(),
                null_mut(),
                null_mut(),
                std::ptr::null(),
            )
        };
        assert!(
            !window.is_null(),
            "create foreground test window: {}",
            std::io::Error::last_os_error()
        );
        let target = Self(window);
        target.activate();
        target
    }
    fn activate(&self) {
        use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
            SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP,
            MOUSEINPUT,
        };
        unsafe {
            SetWindowPos(
                self.0,
                HWND_TOPMOST,
                0,
                0,
                0,
                0,
                SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
            );
            SetForegroundWindow(self.0);
            SetCursorPos(130, 140);
        }
        // A real click supplies the user interaction required by Windows's
        // foreground policy before allowing the child to open its tray menu.
        let inputs = [MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP].map(|flags| INPUT {
            r#type: INPUT_MOUSE,
            Anonymous: INPUT_0 {
                mi: MOUSEINPUT {
                    dwFlags: flags,
                    ..Default::default()
                },
            },
        });
        assert_eq!(
            unsafe { SendInput(2, inputs.as_ptr(), std::mem::size_of::<INPUT>() as i32) },
            2
        );
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            pump_messages();
            if unsafe { GetForegroundWindow() } == self.0 {
                unsafe {
                    SetWindowPos(
                        self.0,
                        HWND_NOTOPMOST,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
                    );
                }
                break;
            }
            assert!(
                Instant::now() < deadline,
                "test target did not receive foreground focus"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}
impl Drop for ForegroundTarget {
    fn drop(&mut self) {
        unsafe {
            DestroyWindow(self.0);
        }
    }
}
fn pump_messages() {
    let mut message = MSG::default();
    while unsafe { PeekMessageW(&mut message, null_mut(), 0, 0, PM_REMOVE) } != 0 {
        unsafe {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
}

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
    let target = ForegroundTarget::new();
    let foreground = target.0;
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

fn choose_tray_item(
    window: windows_sys::Win32::Foundation::HWND,
    down: usize,
    target: &ForegroundTarget,
) {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, VK_DOWN, VK_RETURN,
    };
    target.activate();
    let mut child_pid = 0;
    let child_thread = unsafe { GetWindowThreadProcessId(window, &mut child_pid) };
    assert_ne!(
        unsafe { AllowSetForegroundWindow(child_pid) },
        0,
        "grant tray foreground permission: {}",
        std::io::Error::last_os_error()
    );
    assert_ne!(
        unsafe { PostMessageW(window, WM_APP + 1, 0, WM_RBUTTONUP as isize) },
        0
    );
    let popup_class: Vec<u16> = "#32768".encode_utf16().chain(Some(0)).collect();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        pump_messages();
        let popup = unsafe { FindWindowW(popup_class.as_ptr(), null_mut()) };
        let mut popup_pid = 0;
        if !popup.is_null() {
            unsafe {
                GetWindowThreadProcessId(popup, &mut popup_pid);
            }
        }
        let mut gui = GUITHREADINFO {
            cbSize: std::mem::size_of::<GUITHREADINFO>() as u32,
            ..Default::default()
        };
        if popup_pid == child_pid
            && unsafe { IsWindowVisible(popup) } != 0
            && unsafe { GetGUIThreadInfo(child_thread, &mut gui) } != 0
            && gui.flags & GUI_POPUPMENUMODE != 0
            && gui.hwndMenuOwner == window
            && unsafe { GetForegroundWindow() } == window
        {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "tray popup not ready: popup_pid={popup_pid}, child_pid={child_pid}, menu_flags={}, menu_owner={:?}, foreground={:?}, expected={window:?}",
            gui.flags, gui.hwndMenuOwner, unsafe { GetForegroundWindow() }
        );
        std::thread::sleep(Duration::from_millis(20));
    }
    // Popup menus begin without a keyboard selection; the first Down selects
    // Show subtitles, then subsequent Downs skip the separator.
    let keys = std::iter::repeat_n(VK_DOWN, down + 1).chain(std::iter::once(VK_RETURN));
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
    let target = ForegroundTarget::new();
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
    let wait_ready = |client: &mut Demo, expected: usize| {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            pump_messages();
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
    choose_tray_item(window, 1, &target);
    wait_ready(&mut client, 2);
    assert!(
        overlay_window().is_null(),
        "windowless restart created subtitle window"
    );
    choose_tray_item(window, 2, &target);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        pump_messages();
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
