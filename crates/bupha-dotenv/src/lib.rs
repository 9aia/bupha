//! Dotenv impl of `Secrets` — a `KEY=VAL` file, written 0600.
//! Last store in the chain: headless boxes without a desktop
//! keyring land here.
//!
//! **LEGACY (T-056).** The canonical secrets story is the cuora
//! broker — `bupha-secret-cuora` resolves `cuora://bupha/<NAME>`
//! through the `cuora` CLI ahead of this store in the chain. This
//! impl (and the `~/.config/bupha/env` file the provider unit's
//! `env_file` loads) stays as a fallback for hosts without cuora;
//! plaintext-at-rest files are exactly what cuora exists to retire.

use bupha_secret::Secrets;
use std::io::{self, Write};
use std::path::PathBuf;

pub struct DotenvSecrets {
    pub path: PathBuf,
}

impl Secrets for DotenvSecrets {
    fn get(&self, name: &str) -> Option<String> {
        let f = std::fs::read_to_string(&self.path).ok()?;
        f.lines().find_map(|l| {
            let (k, v) = l.split_once('=')?;
            (k.trim() == name && !v.trim().is_empty()).then(|| v.trim().to_string())
        })
    }
    fn set(&self, name: &str, value: &str) -> io::Result<()> {
        let mut lines: Vec<String> = std::fs::read_to_string(&self.path)
            .unwrap_or_default()
            .lines()
            .map(String::from)
            .collect();
        let hit = lines
            .iter()
            .position(|l| l.split_once('=').map(|(k, _)| k.trim()) == Some(name));
        match hit {
            Some(i) => lines[i] = format!("{name}={value}"),
            None => lines.push(format!("{name}={value}")),
        }
        if let Some(d) = self.path.parent() {
            std::fs::create_dir_all(d)?;
        }
        // 0600 from the first write — never a world-readable window.
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        opts.open(&self.path)?
            .write_all((lines.join("\n") + "\n").as_bytes())?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&self.path, std::fs::Permissions::from_mode(0o600))?;
        }
        Ok(())
    }
}
