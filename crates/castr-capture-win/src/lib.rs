//! Windows-only capture: Desktop Duplication video and WASAPI loopback audio.
#![cfg(windows)]
pub mod dxgi;
pub use dxgi::DesktopCapture;
