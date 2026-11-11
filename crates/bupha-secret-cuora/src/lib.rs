//! Cuora impl of `Secrets` — the canonical secrets story (T-056).
//!
//! Each bupha secret NAME resolves to a fixed cuora ref:
//! `cuora://bupha/<NAME>` — e.g. `BUPHA_WH_9AIA_BUPHA` maps to
//! `cuora://bupha/BUPHA_WH_9AIA_BUPHA`. Values are fetched through
//! the `cuora` CLI (`get`/`put`) so audit events, guardrails, and
//! vault policy apply exactly as for every other consumer; bupha
//! never stores a value itself.
//!
//! `get` shells `cuora get` per lookup. Secrets resolve once at
//! watcher/driver boot (a handful of names per fleet), so a
//! per-call process is cheaper than a persistent `cuora serve`
//! session and needs no shared mutable state. A missing `cuora`
//! binary, a denied op, or an absent ref all answer `None` —
//! "not here" — so the caller's chain falls through to the legacy
//! stores exactly like a keyring miss on a headless box.
//!
//! Env knobs: `BUPHA_CUORA_BIN` overrides the binary path (tests
//! stub it); `CUORA_*` and `--config` resolution stay cuora's own.
//! `CUORA_ENV`/`--env` picks the vault env — bupha does not pin one.

use bupha_secret::Secrets;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

/// Ref namespace — every bupha secret name lives under
/// `cuora://bupha/…` so `cuora list` shows the whole fleet surface.
pub const NAMESPACE: &str = "bupha";

/// `Secrets` over the `cuora` CLI. Stateless — each call spawns a
/// fresh `cuora get`/`put`, inheriting cuora's own config search
/// (`$CUORA_CONFIG` → `./cuora.toml` → `~/.config/cuora/cuora.toml`).
pub struct CuoraSecrets {
    /// The `cuora` binary — `BUPHA_CUORA_BIN` when set, else `cuora`
    /// resolved on PATH.
    pub bin: PathBuf,
}

impl Default for CuoraSecrets {
    fn default() -> Self {
        Self {
            bin: std::env::var_os("BUPHA_CUORA_BIN")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from("cuora")),
        }
    }
}

/// Build the ref for a secret name. A free fn so tests pin the
/// mapping without constructing the store.
///
/// `BUPHA_WH_9AIA_BUPHA` → `cuora://bupha/BUPHA_WH_9AIA_BUPHA`.
/// Names are env-var-shaped (`[A-Z0-9_]`) which cuora's ref grammar
/// accepts verbatim — no slugging, the name IS the ref tail, so
/// `cuora put cuora://bupha/<NAME>` is the migration path with
/// zero translation.
pub fn rref(name: &str) -> String {
    format!("cuora://{NAMESPACE}/{name}")
}

impl Secrets for CuoraSecrets {
    fn get(&self, name: &str) -> Option<String> {
        let o = Command::new(&self.bin)
            .args(["--actor", "bupha", "get", &rref(name)])
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .output()
            .ok()?;
        if !o.status.success() {
            return None;
        }
        // `cuora get` prints the value via println! — strip exactly
        // one trailing newline (and a CR before it); interior
        // whitespace is part of the value.
        let mut s = String::from_utf8_lossy(&o.stdout).into_owned();
        if let Some(stripped) = s.strip_suffix('\n') {
            s = stripped.strip_suffix('\r').unwrap_or(stripped).to_string();
        }
        (!s.is_empty()).then_some(s)
    }

    /// Deploy-time migration: `cuora put` reads the value on stdin
    /// (never argv — argv lands in shell history and `/proc`).
    fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        let mut c = Command::new(&self.bin)
            .args(["--actor", "bupha", "put", &rref(name)])
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = c.stdin.take() {
            stdin.write_all(value.as_bytes())?;
        }
        let status = c.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(std::io::Error::new(
                std::io::ErrorKind::Other,
                format!("cuora put {} exited {status}", rref(name)),
            ))
        }
    }
}
