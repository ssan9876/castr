//! Windows-only capture: Desktop Duplication video and WASAPI loopback audio.
#![cfg(windows)]
pub mod cursor;
pub mod dxgi;
pub mod outputs;
pub mod wasapi;
pub use dxgi::DesktopCapture;
pub use outputs::{outputs, Output};
pub use wasapi::LoopbackCapture;
