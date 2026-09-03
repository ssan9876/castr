//! `castr-sender diagnose`: find and, with consent, remove the local causes of
//! Miracast disconnects on this machine.
//!
//! Layering matters here. `facts` and `rules` are pure: they hold data and
//! judgment and are tested on every platform against output captured from a
//! real machine. `collect` and `fix` are the only parts that touch Windows.

pub mod facts;
pub mod rules;

// Later tasks (`render`) consume `Facts`, `Finding`, and `Severity` through
// these re-exports.
#[allow(unused_imports)]
pub use facts::Facts;
#[allow(unused_imports)]
pub use rules::{Finding, Severity};
