//! A Miracast (Wi-Fi Display) sink: Wi-Fi Direct group owner, RTSP session,
//! MPEG-TS over RTP, decoded by the same pipeline castr's own protocol uses.
//! Linux only in its radio layer; on other targets the pure layers still build
//! so the workspace compiles everywhere.
//!
//! The parsing layers (`wfd`, `rtsp`, `ts`, `rtp`, `dhcp`) are pure and are
//! declared on every platform so their tests run in the Windows workspace
//! suite; only the parts that own sockets are Linux-gated.

// Declared as each task creates its file. The parsing and state-machine
// layers end up ungated so their tests run everywhere; only `sink`, which owns
// the supplicant and the sockets, is Linux-only.
pub mod dhcp;
pub mod rtp;
pub mod rtsp;
pub mod ts;
pub mod wfd;
// Task 7: pub mod p2p;
// Task 8: pub mod session;
// Task 10: #[cfg(target_os = "linux")] pub mod sink;
