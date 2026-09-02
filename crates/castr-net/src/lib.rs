//! See docs/superpowers/specs/2026-09-01-castr-core-design.md
pub mod identity;
pub mod tls;
pub mod transport;
pub use identity::*;
pub use tls::*;
pub use transport::*;
