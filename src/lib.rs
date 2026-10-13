pub mod audio;
pub mod config;
pub mod mock;
pub mod output;
pub mod overlay;
pub mod platform;
pub mod protocol;
pub mod session;
pub mod tray;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlEvent {
    Start,
    Stop,
    Cancel,
    Shutdown,
    Restart,
}
