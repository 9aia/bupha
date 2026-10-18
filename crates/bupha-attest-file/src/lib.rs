//! File adapter of `Attest` — writes an in-toto/SLSA-shaped predicate
//! JSON per finished run to `<dir>/<repo>-<sha12>.json`. Unsigned v1:
//! provenance is *recorded*, signing (`bupha-attest-sigstore`) is the
//! follow-up adapter.

use bupha_attest::{Attest, RunAttestation};
use std::io::{self, Write};
use std::path::PathBuf;

/// Writes predicate files under `dir` (created on demand).
pub struct FileAttest {
    /// Destination directory for `*.json` predicates.
    pub dir: PathBuf,
}

impl Attest for FileAttest {
    fn attest(&self, a: &RunAttestation) -> io::Result<PathBuf> {
        std::fs::create_dir_all(&self.dir)?;
        let clean = |s: &str| s.replace(['/', '"', '\\'], "-");
        let sha12 = &a.sha[..12.min(a.sha.len())];
        let path = self.dir.join(format!("{}-{}.json", clean(&a.repo), sha12));
        let json = format!(
            r#"{{"_type":"https://in-toto.io/Statement/v1",
"subject":[{{"name":"{}","digest":{{"gitCommit":"{}"}}}}],
"predicateType":"https://bupha.dev/attestation/run/v1",
"predicate":{{"runner":"{}","status":"{}","wall_secs":{}}}}}
"#,
            clean(&a.repo),
            clean(&a.sha),
            clean(&a.runner),
            clean(&a.status),
            a.wall_secs
        );
        let mut f = std::fs::File::create(&path)?;
        f.write_all(json.as_bytes())?;
        Ok(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_written() {
        let dir = std::env::temp_dir().join(format!("att-{}", std::process::id()));
        let a = FileAttest { dir: dir.clone() };
        let p = a
            .attest(&RunAttestation {
                sha: "0123456789abcdef".into(),
                repo: "org/repo".into(),
                runner: "shell".into(),
                status: "success".into(),
                wall_secs: 42,
            })
            .unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert!(s.contains("in-toto.io/Statement"));
        assert_eq!(
            p.file_name().unwrap().to_string_lossy(),
            "org-repo-0123456789ab.json"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
