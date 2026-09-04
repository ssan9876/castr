//! The Wi-Fi Display *source* role: what we send when castr is the thing being
//! cast from, rather than the thing being cast to.
//!
//! Each module here mirrors a sink module beside it - `ts_mux` against `ts`,
//! `rtp_pack` against `rtp` - and follows the same rule: bytes in, bytes or
//! actions out, no sockets, so a whole session replays in a test.

pub mod caps;
pub mod lpcm;
pub mod rtp_pack;
pub mod session;
pub mod ts_mux;
