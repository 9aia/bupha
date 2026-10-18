//! Attestation port — signed provenance per run (SLSA-style). Each
//! run emits a predicate: sha, repo, runner, status, wall.

use std::io;
use std::path::PathBuf;

/// Run provenance — what ran, where, outcome.
pub struct RunAttestation {
    pub sha: String,
    pub repo: String,
    pub runner: String,
    pub status: String,
    pub wall_secs: u64,
}

/// Emit a signed attestation for a finished run.
pub trait Attest: Send + Sync {
    /// Write/sign the predicate; return where it landed.
    fn attest(&self, a: &RunAttestation) -> io::Result<PathBuf>;
}
