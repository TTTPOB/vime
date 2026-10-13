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
