//! A read-only, on-demand transcription window. The session owns all recognition state.
use crate::config::{HotkeyMode, OverlayConfig};
use iced::{
    widget::{column, container, row, scrollable, text},
    window, Color, Element, Font, Length, Subscription, Task, Theme,
};
use std::{
    sync::{mpsc, Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UiPhase {
    #[default]
    Idle,
    Connecting,
    Recording,
    Finalizing,
    Delivered,
    Recovered,
    Error,
}
#[derive(Debug, Clone, Default)]
pub struct UiSnapshot {
    pub phase: UiPhase,
    pub text: String,
    pub detail: String,
    pub started: Option<Instant>,
}
#[derive(Debug)]
pub enum DesktopCommand {
    Show(bool),
    Exit,
}
#[derive(Debug, Clone)]
pub enum Message {
    Tick,
    Opened(window::Id),
    Ready,
    Closed(window::Id),
}
struct Overlay {
    receiver: watch::Receiver<UiSnapshot>,
    controls: Arc<Mutex<mpsc::Receiver<DesktopCommand>>>,
    snapshot: UiSnapshot,
    enabled: bool,
    dismissed: bool,
    window: Option<window::Id>,
    font: Font,
    key: String,
    mode: HotkeyMode,
}

pub fn run(
    receiver: watch::Receiver<UiSnapshot>,
    config: &OverlayConfig,
    controls: mpsc::Receiver<DesktopCommand>,
    key: &str,
    mode: HotkeyMode,
) -> anyhow::Result<()> {
    let enabled = config.enabled;
    let key = key.to_owned();
    let font = configured_font(&config.font_family);
    let controls = Arc::new(Mutex::new(controls));
    iced::daemon(
        move || {
            (
                Overlay {
                    snapshot: receiver.borrow().clone(),
                    receiver: receiver.clone(),
                    controls: controls.clone(),
                    enabled,
                    dismissed: false,
                    window: None,
                    font,
                    key: key.clone(),
                    mode,
                },
                Task::none(),
            )
        },
        Overlay::update,
        Overlay::view,
    )
    .subscription(Overlay::subscription)
    .theme(Theme::Dark)
    .style(|_, _| iced::theme::Style {
        background_color: Color::TRANSPARENT,
        text_color: Color::WHITE,
    })
    .default_font(font)
    .title(|_: &Overlay, _: window::Id| String::from("vime subtitles"))
    .run()?;
    Ok(())
}
fn configured_font(family: &str) -> Font {
    let mut database = fontdb::Database::new();
    database.load_system_fonts();
    if database
        .query(&fontdb::Query {
            families: &[fontdb::Family::Name(family)],
            ..Default::default()
        })
        .is_some()
    {
        Font::with_name(Box::leak(family.to_owned().into_boxed_str()))
    } else {
        tracing::warn!(
            font_family = family,
            "overlay font unavailable; using system sans-serif"
        );
        Font::DEFAULT
    }
}
impl Overlay {
    fn subscription(&self) -> Subscription<Message> {
        Subscription::batch([
            window::close_events().map(Message::Closed),
            iced::time::every(Duration::from_millis(if self.window.is_some() {
                50
            } else {
                150
            }))
            .map(|_| Message::Tick),
        ])
    }
    fn update(&mut self, message: Message) -> Task<Message> {
        match message {
            Message::Closed(id) => {
                if self.window == Some(id) {
                    self.window = None;
                    self.dismissed = true;
                }
                return Task::none();
            }
            Message::Ready => return Task::none(),
            Message::Opened(id) => return prepare_window(id),
            Message::Tick => {}
        }
        while let Ok(command) = self
            .controls
            .lock()
            .expect("desktop command mutex poisoned")
            .try_recv()
        {
            match command {
                DesktopCommand::Exit => return iced::exit(),
                DesktopCommand::Show(enabled) => {
                    self.enabled = enabled;
                    self.dismissed = false;
                }
            }
        }
        let mut text_changed = false;
        if self.receiver.has_changed().unwrap_or(false) {
            let next = self.receiver.borrow_and_update().clone();
            text_changed = self.snapshot.text != next.text;
            if next.phase == UiPhase::Idle || next.started != self.snapshot.started {
                self.dismissed = false;
            }
            self.snapshot = next;
        }
        let visible = self.enabled && !self.dismissed && self.snapshot.phase != UiPhase::Idle;
        match (visible, self.window) {
            (true, None) => {
                let settings = window::Settings {
                    size: iced::Size::new(480.0, 190.0),
                    visible: !cfg!(windows),
                    resizable: false,
                    decorations: false,
                    transparent: true,
                    level: window::Level::AlwaysOnTop,
                    position: window::Position::Centered,
                    ..Default::default()
                };
                #[cfg(windows)]
                let settings = {
                    let mut settings = settings;
                    settings.platform_specific.skip_taskbar = true;
                    settings
                };
                let (id, task) = window::open(settings);
                self.window = Some(id);
                task.map(Message::Opened)
            }
            (false, Some(id)) => {
                self.window = None;
                window::close(id)
            }
            _ if text_changed => iced::widget::operation::snap_to_end("transcript"),
            _ => Task::none(),
        }
    }
    fn view(&self, _: window::Id) -> Element<'_, Message> {
        view_snapshot(&self.snapshot, self.font, &self.key, self.mode)
    }
}

pub fn view_snapshot<'a>(
    snapshot: &'a UiSnapshot,
    font: Font,
    key: &str,
    mode: HotkeyMode,
) -> Element<'a, Message> {
    let (label, accent) = match snapshot.phase {
        UiPhase::Idle => ("READY", Color::from_rgb8(135, 150, 169)),
        UiPhase::Connecting => ("CONNECTING", Color::from_rgb8(156, 175, 255)),
        UiPhase::Recording => ("LISTENING", Color::from_rgb8(106, 225, 190)),
        UiPhase::Finalizing => ("FINISHING", Color::from_rgb8(156, 175, 255)),
        UiPhase::Delivered => ("DONE", Color::from_rgb8(106, 225, 190)),
        UiPhase::Recovered => ("RECOVERED", Color::from_rgb8(245, 199, 112)),
        UiPhase::Error => ("NEEDS ATTENTION", Color::from_rgb8(255, 151, 151)),
    };
    let seconds = snapshot
        .started
        .map(|time| time.elapsed().as_secs())
        .unwrap_or(0);
    let timer = if matches!(
        snapshot.phase,
        UiPhase::Connecting | UiPhase::Recording | UiPhase::Finalizing
    ) {
        format!("{:02}:{:02}", seconds / 60, seconds % 60)
    } else {
        String::new()
    };
    let header = row![
        text("●").size(11).color(accent),
        text(label).size(11).color(accent),
        iced::widget::Space::new().width(Length::Fill),
        text(timer).size(12).color(Color::from_rgb8(135, 150, 169))
    ]
    .spacing(8)
    .align_y(iced::Alignment::Center);
    let content = if snapshot.text.is_empty() {
        "Your words appear here · 请开始说话"
    } else {
        &snapshot.text
    };
    let transcript = scrollable(
        container(
            text(content)
                .width(Length::Fill)
                .font(font)
                .size(21)
                .line_height(1.45)
                .shaping(iced::widget::text::Shaping::Advanced)
                .color(Color::from_rgb8(238, 242, 248)),
        )
        .width(Length::Fill)
        .padding(iced::Padding {
            right: 14.0,
            ..Default::default()
        }),
    )
    .id("transcript")
    .anchor_bottom()
    .height(Length::Fixed(92.0));
    let footer = row![
        text(if snapshot.detail.is_empty() {
            format!(
                "{} {key} 结束 · Esc 取消",
                if mode == HotkeyMode::PushToTalk {
                    "松开"
                } else {
                    "再按"
                }
            )
        } else {
            snapshot.detail.clone()
        })
        .font(font)
        .shaping(iced::widget::text::Shaping::Advanced)
        .size(11)
        .color(Color::from_rgb8(135, 150, 169)),
        iced::widget::Space::new().width(Length::Fill),
        text("vime").size(12).color(Color::from_rgb8(135, 150, 169))
    ];
    container(column![header, transcript, footer].spacing(12))
        .padding(18)
        .width(Length::Fill)
        .height(Length::Fill)
        .style(|_| container::Style {
            background: Some(Color::from_rgba8(19, 25, 35, 0.97).into()),
            border: iced::Border {
                color: Color::from_rgba8(144, 163, 190, 0.22),
                width: 1.0,
                radius: 16.0.into(),
            },
            ..Default::default()
        })
        .into()
}

#[cfg(not(windows))]
fn prepare_window(_: window::Id) -> Task<Message> {
    Task::none()
}
#[cfg(windows)]
fn prepare_window(id: window::Id) -> Task<Message> {
    window::run(id, |native| {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        use windows_sys::Win32::UI::WindowsAndMessaging::*;
        if let Ok(handle) = native.window_handle() {
            if let RawWindowHandle::Win32(handle) = handle.as_raw() {
                let hwnd = handle.hwnd.get() as windows_sys::Win32::Foundation::HWND;
                unsafe {
                    let style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE);
                    SetWindowLongPtrW(
                        hwnd,
                        GWL_EXSTYLE,
                        style | WS_EX_NOACTIVATE as isize | WS_EX_TOOLWINDOW as isize,
                    );
                    let mut work = std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>();
                    if SystemParametersInfoW(SPI_GETWORKAREA, 0, &mut work as *mut _ as *mut _, 0)
                        != 0
                    {
                        let mut bounds = std::mem::zeroed::<windows_sys::Win32::Foundation::RECT>();
                        GetWindowRect(hwnd, &mut bounds);
                        let width = bounds.right - bounds.left;
                        let height = bounds.bottom - bounds.top;
                        SetWindowPos(
                            hwnd,
                            HWND_TOPMOST,
                            work.left + (work.right - work.left - width) / 2,
                            work.bottom - height - 36,
                            0,
                            0,
                            SWP_NOSIZE | SWP_NOACTIVATE | SWP_FRAMECHANGED,
                        );
                    }
                    ShowWindow(hwnd, SW_SHOWNOACTIVATE);
                }
            }
        }
    })
    .map(|_| Message::Ready)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unavailable_font_uses_system_sans_serif() {
        assert_eq!(
            configured_font("vime-nonexistent-font-for-fallback-test"),
            Font::DEFAULT
        );
    }
}
