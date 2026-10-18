//! Run-history adapter — appends one JSONL record per finished run
//! to `<worktree.base>/history.jsonl`. Read path: `bupha runs`
//! (or `tail` the file). Keeps the last records bounded by rotation.

use std::io::{self, Write};
use std::path::Path;

/// One completed run.
pub struct RunRecord {
    /// Pushed revision.
    pub sha: String,
    /// Watched repo ("owner/name").
    pub repo: String,
    /// Outcome ("success" | "failure" | "cancelled").
    pub status: String,
    /// Wall time in seconds.
    pub wall_secs: u64,
}

/// Append a run record. Failures are non-fatal for the pipeline —
/// history is observability, not correctness.
pub fn record(path: &Path, r: &RunRecord) -> io::Result<()> {
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?;
    // Fields come from the forge payload/config — sanitize the two
    // characters that could break the JSONL shape.
    let clean = |s: &str| s.replace(['"', '\\', '\n'], "_");
    writeln!(
        f,
        r#"{{"sha":"{}","repo":"{}","status":"{}","wall_secs":{}}}"#,
        clean(&r.sha),
        clean(&r.repo),
        clean(&r.status),
        r.wall_secs
    )
}
