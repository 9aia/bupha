//! Sigstore adapter of `Attest` — writes the predicate via
//! `bupha-attest-file` then `cosign sign-blob`s it, storing `<p>.sig`
//! alongside. Requires `cosign` on PATH and a signing key (path or
//! env://). Keyless signing works when cosign is configured for it.

use bupha_attest::{Attest, RunAttestation};
use bupha_attest_file::FileAttest;
use std::io::{self, ErrorKind};
use std::path::PathBuf;
use std::process::Command;

/// Signs predicate files with cosign.
pub struct SigstoreAttest {
    /// Where predicates land (shared with the file adapter).
    pub dir: PathBuf,
    /// cosign key reference — file path, `env://NAME`, or
    /// `k8s://`/`awskms://` style refs cosign understands.
    /// Empty = keyless (Fulcio) signing.
    pub key: String,
}

impl Attest for SigstoreAttest {
    fn attest(&self, a: &RunAttestation) -> io::Result<PathBuf> {
        let pred = FileAttest {
            dir: self.dir.clone(),
        }
        .attest(a)?;
        let sig = pred.with_extension("sig");
        let mut cmd = Command::new("cosign");
        cmd.args(["sign-blob", "--yes"])
            .arg("--output-signature")
            .arg(&sig);
        if !self.key.is_empty() {
            cmd.args(["--key", &self.key]);
        }
        cmd.arg(&pred);
        let out = cmd.output().map_err(|e| {
            if e.kind() == ErrorKind::NotFound {
                io::Error::other("cosign not on PATH — install sigstore/cosign")
            } else {
                e
            }
        })?;
        if out.status.success() {
            Ok(sig)
        } else {
            Err(io::Error::other(format!(
                "cosign: {}",
                String::from_utf8_lossy(&out.stderr)
            )))
        }
    }
}
