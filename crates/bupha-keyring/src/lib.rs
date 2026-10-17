//! Keyring impl of `Secrets` — shells out to `secret-tool`
//! (libsecret). Headless boxes without dbus simply miss every
//! lookup and the caller falls through to the file store.

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
        c.wait().map(|s| {
            if s.success() {
                Ok(())
            } else {
                Err(std::io::Error::other("secret-tool store failed"))
            }
        })?
    }
}
