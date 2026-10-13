//! Windows notification-area controls, owned by a dedicated message-loop thread.
use anyhow::Result;
use tokio::sync::mpsc::UnboundedSender;

#[derive(Debug, Clone, Copy)]
pub enum TrayCommand {
    Restart,
    Stop,
    ToggleOverlay(bool),
}

pub struct Tray {
    #[cfg(windows)]
    window: usize,
    #[cfg(windows)]
    thread: Option<std::thread::JoinHandle<()>>,
}

pub fn start(
    enabled: bool,
    overlay_enabled: bool,
    hotkey: &str,
    sender: UnboundedSender<TrayCommand>,
) -> Result<Tray> {
    #[cfg(windows)]
    {
        if enabled {
            return windows::start(overlay_enabled, hotkey, sender);
        }
        Ok(Tray {
            window: 0,
            thread: None,
        })
    }
    #[cfg(not(windows))]
    {
        let _ = (enabled, overlay_enabled, hotkey, sender);
        Ok(Tray {})
    }
}

#[cfg(windows)]
impl Drop for Tray {
    fn drop(&mut self) {
        if let Some(thread) = self.thread.take() {
            unsafe {
                windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW(
                    self.window as _,
                    windows_sys::Win32::UI::WindowsAndMessaging::WM_CLOSE,
                    0,
                    0,
                );
            }
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
mod windows {
    use super::*;
    use anyhow::{anyhow, Context};
    use std::{cell::RefCell, mem::size_of, ptr::null_mut};
    use windows_sys::Win32::{
        Foundation::*,
        System::LibraryLoader::GetModuleHandleW,
        UI::{Shell::*, WindowsAndMessaging::*},
    };

    const CALLBACK: u32 = WM_APP + 1;
    const SHOW: usize = 1;
    const RESTART: usize = 2;
    const STOP: usize = 3;

    struct State {
        sender: UnboundedSender<TrayCommand>,
        overlay: bool,
        icon: NOTIFYICONDATAW,
        taskbar_created: u32,
    }

    thread_local! { static STATE: RefCell<Option<State>> = const { RefCell::new(None) }; }

    fn wide(value: &str) -> Vec<u16> {
        value.encode_utf16().chain(Some(0)).collect()
    }

    pub(super) fn start(
        overlay: bool,
        hotkey: &str,
        sender: UnboundedSender<TrayCommand>,
    ) -> Result<Tray> {
        let tooltip = format!("vime · {hotkey}");
        let (ready_tx, ready_rx) = std::sync::mpsc::sync_channel(1);
        let thread = std::thread::Builder::new()
            .name("vime-tray".into())
            .spawn(move || unsafe {
                match initialize(overlay, &tooltip, sender) {
                    Ok(window) => {
                        if ready_tx.send(Ok(window as usize)).is_ok() {
                            let mut message: MSG = std::mem::zeroed();
                            loop {
                                let result = GetMessageW(&mut message, null_mut(), 0, 0);
                                if result <= 0 {
                                    if result < 0 {
                                        tracing::error!(
                                            "tray message loop: {}",
                                            std::io::Error::last_os_error()
                                        );
                                    }
                                    break;
                                }
                                TranslateMessage(&message);
                                DispatchMessageW(&message);
                            }
                        }
                        STATE.with(|cell| {
                            if let Some(state) = cell.borrow_mut().take() {
                                Shell_NotifyIconW(NIM_DELETE, &state.icon);
                                DestroyIcon(state.icon.hIcon);
                            }
                        });
                        DestroyWindow(window);
                        UnregisterClassW(
                            wide("VimeTrayWindow").as_ptr(),
                            GetModuleHandleW(std::ptr::null()),
                        );
                    }
                    Err(error) => {
                        let _ = ready_tx.send(Err(error));
                    }
                }
            })
            .context("start tray thread")?;
        match ready_rx
            .recv()
            .context("tray thread exited during startup")?
        {
            Ok(window) => Ok(Tray {
                window,
                thread: Some(thread),
            }),
            Err(error) => {
                let _ = thread.join();
                Err(error)
            }
        }
    }

    unsafe fn initialize(
        overlay: bool,
        tooltip: &str,
        sender: UnboundedSender<TrayCommand>,
    ) -> Result<HWND> {
        let instance = GetModuleHandleW(std::ptr::null());
        let class_name = wide("VimeTrayWindow");
        let class = WNDCLASSW {
            lpfnWndProc: Some(window_proc),
            hInstance: instance,
            lpszClassName: class_name.as_ptr(),
            ..std::mem::zeroed()
        };
        if RegisterClassW(&class) == 0 {
            return Err(std::io::Error::last_os_error()).context("register tray window");
        }
        // An invisible top-level window receives Explorer's TaskbarCreated broadcast.
        let window = CreateWindowExW(
            0,
            class_name.as_ptr(),
            class_name.as_ptr(),
            0,
            0,
            0,
            0,
            0,
            null_mut(),
            null_mut(),
            instance,
            std::ptr::null(),
        );
        if window.is_null() {
            UnregisterClassW(class_name.as_ptr(), instance);
            return Err(std::io::Error::last_os_error()).context("create tray window");
        }
        let icon = make_icon(instance);
        if icon.is_null() {
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), instance);
            return Err(std::io::Error::last_os_error()).context("create tray icon");
        }
        let mut notification: NOTIFYICONDATAW = std::mem::zeroed();
        notification.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        notification.hWnd = window;
        notification.uID = 1;
        notification.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        notification.uCallbackMessage = CALLBACK;
        notification.hIcon = icon;
        for (target, source) in notification
            .szTip
            .iter_mut()
            .take(127)
            .zip(tooltip.encode_utf16())
        {
            *target = source;
        }
        if Shell_NotifyIconW(NIM_ADD, &notification) == 0 {
            DestroyIcon(icon);
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), instance);
            return Err(anyhow!("Windows could not add the vime notification icon"));
        }
        let taskbar_created = RegisterWindowMessageW(wide("TaskbarCreated").as_ptr());
        if taskbar_created == 0 {
            let error = std::io::Error::last_os_error();
            Shell_NotifyIconW(NIM_DELETE, &notification);
            DestroyIcon(icon);
            DestroyWindow(window);
            UnregisterClassW(class_name.as_ptr(), instance);
            return Err(error).context("register Explorer restart notification");
        }
        STATE.with(|cell| {
            *cell.borrow_mut() = Some(State {
                sender,
                overlay,
                icon: notification,
                taskbar_created,
            })
        });
        Ok(window)
    }

    unsafe fn make_icon(instance: HINSTANCE) -> HICON {
        let mut pixels = [0u8; 32 * 32 * 4];
        let mut mask = [255u8; 32 * 4];
        // Three mint waveform strokes remain legible at notification-area sizes.
        for y in 0usize..32 {
            for x in 0usize..32 {
                let stroke = ((6..10).contains(&x) && (11..21).contains(&y))
                    || ((14..18).contains(&x) && (5..27).contains(&y))
                    || ((22..26).contains(&x) && (9..23).contains(&y));
                if stroke {
                    let index = (y * 32 + x) * 4;
                    pixels[index..index + 4].copy_from_slice(&[173, 218, 76, 255]);
                    mask[y * 4 + x / 8] &= !(0x80 >> (x % 8));
                }
            }
        }
        CreateIcon(instance, 32, 32, 1, 32, mask.as_ptr(), pixels.as_ptr())
    }

    unsafe extern "system" fn window_proc(
        window: HWND,
        message: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        if message == WM_CLOSE {
            DestroyWindow(window);
            return 0;
        }
        if message == WM_DESTROY {
            PostQuitMessage(0);
            return 0;
        }
        STATE.with(|cell| {
            let mut borrowed = cell.borrow_mut();
            let Some(state) = borrowed.as_mut() else {
                drop(borrowed);
                return DefWindowProcW(window, message, wparam, lparam);
            };
            if message == state.taskbar_created && state.taskbar_created != 0 {
                if Shell_NotifyIconW(NIM_ADD, &state.icon) == 0 {
                    tracing::error!("could not restore vime tray icon after Explorer restarted");
                }
                return 0;
            }
            if message == CALLBACK
                && (lparam as u32 == WM_RBUTTONUP
                    || lparam as u32 == WM_CONTEXTMENU
                    || lparam as u32 == WM_LBUTTONUP)
            {
                let overlay = state.overlay;
                drop(borrowed);
                let menu = CreatePopupMenu();
                if menu.is_null() {
                    tracing::error!("create tray menu: {}", std::io::Error::last_os_error());
                    return 0;
                }
                AppendMenuW(
                    menu,
                    MF_STRING | if overlay { MF_CHECKED } else { MF_UNCHECKED },
                    SHOW,
                    wide("Show subtitles").as_ptr(),
                );
                AppendMenuW(menu, MF_SEPARATOR, 0, std::ptr::null());
                AppendMenuW(menu, MF_STRING, RESTART, wide("Restart").as_ptr());
                AppendMenuW(menu, MF_STRING, STOP, wide("Stop").as_ptr());
                let mut point: POINT = std::mem::zeroed();
                GetCursorPos(&mut point);
                SetForegroundWindow(window);
                let choice = TrackPopupMenu(
                    menu,
                    TPM_RETURNCMD | TPM_NONOTIFY | TPM_RIGHTBUTTON,
                    point.x,
                    point.y,
                    0,
                    window,
                    std::ptr::null(),
                ) as usize;
                DestroyMenu(menu);
                PostMessageW(window, WM_NULL, 0, 0);
                let mut borrowed = cell.borrow_mut();
                let Some(state) = borrowed.as_mut() else {
                    return 0;
                };
                let command = match choice {
                    SHOW => {
                        state.overlay = !state.overlay;
                        Some(TrayCommand::ToggleOverlay(state.overlay))
                    }
                    RESTART => Some(TrayCommand::Restart),
                    STOP => Some(TrayCommand::Stop),
                    _ => None,
                };
                if let Some(command) = command {
                    tracing::info!(?command, "tray command selected");
                    let _ = state.sender.send(command);
                }
                return 0;
            }
            drop(borrowed);
            DefWindowProcW(window, message, wparam, lparam)
        })
    }
}
