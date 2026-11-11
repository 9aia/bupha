//! Keyring impl of `Secrets` — shells out to `secret-tool`
//! (libsecret). Headless boxes without dbus simply miss every
//! lookup and the caller falls through to the file store.
//!
//! **LEGACY (T-056).** The canonical secrets story is the cuora
//! broker — `bupha-secret-cuora` resolves `cuora://bupha/<NAME>`
//! through the `cuora` CLI ahead of this store in the chain. This
//! impl stays as a fallback for hosts without cuora; new secrets
//! belong in cuora, not the keyring.

use bupha_secret::Secrets;
use std::io::Write;
use std::process::{Command, Stdio};

pub struct KeyringSecrets {
    /// secret-tool `service` attribute — groups all bupha entries.
    pub service: String,
}

impl Default for KeyringSecrets {
    fn default() -> Self {
        Self {
            service: "bupha".into(),
        }
    }
}

impl Secrets for KeyringSecrets {
    fn get(&self, name: &str) -> Option<String> {
        let o = Command::new("secret-tool")
            .args(["lookup", "service", &self.service, "name", name])
            .output()
            .ok()?;
        o.status
            .success()
            .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|v| !v.is_empty())
    }
    fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        let mut c = Command::new("secret-tool")
            .args([
                "store",
                "--label",
                &format!("{}: {name}", self.service),
                "service",
                &self.service,
                "name",
                name,
            ])
            .stdin(Stdio::piped())
            .spawn()?;
        c.stdin.as_mut().unwrap().write_all(value.as_bytes())?;
        // A failed wait() is reported the same way as a non-zero exit.
        if !c.wait().map(|s| s.success()).unwrap_or(false) {
            return Err(std::io::Error::other("secret-tool store failed"));
        }
        Ok(())
    }
}
