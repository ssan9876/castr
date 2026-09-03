//! `castr-sender diagnose`: find and, with consent, remove the local causes of
//! Miracast disconnects on this machine.
//!
//! Layering matters here. `facts` and `rules` are pure: they hold data and
//! judgment and are tested on every platform against output captured from a
//! real machine. `collect` and `fix` are the only parts that touch Windows.

#[cfg(windows)]
pub mod collect;
pub mod facts;
pub mod fix;
pub mod render;
pub mod rules;

// Later tasks consume `Facts`, `Finding`, `Severity`, and `FixId` through
// these re-exports.
#[allow(unused_imports)]
pub use facts::Facts;
#[allow(unused_imports)]
pub use rules::{Finding, FixId, Severity};

/// Runs the health check. Returns the process exit code: 0 when nothing is
/// wrong, 1 when anything warned, failed or could not be read.
#[cfg(not(windows))]
pub fn run(_apply_fixes: bool) -> anyhow::Result<i32> {
    anyhow::bail!("diagnose is Windows only")
}

#[cfg(windows)]
pub fn run(apply_fixes: bool) -> anyhow::Result<i32> {
    let facts = collect::facts();
    let findings = rules::analyse(&facts);
    print!("{}", render::report(&findings, &facts));
    if apply_fixes {
        fix::prompt_and_apply(&findings, &facts)?;
    }
    Ok(match rules::Severity::worst_of(&findings) {
        rules::Severity::Ok | rules::Severity::Info => 0,
        _ => 1,
    })
}
