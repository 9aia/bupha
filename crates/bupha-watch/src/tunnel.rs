//! Tunnel support for the standalone/dev-mode watcher (ADR-0007) —
//! makes the local webhook listener reachable. Production ingress is
//! a peregrin campaign listener; this module is the single-box
//! fallback, inlined from the retired `bupha-tunnel` (port) and
//! `bupha-cloudflared` (impl) crates (T-077). Additional providers
//! (bore, tailscale, direct/no-op) impl [`Tunnel`] here if ever
//! needed.

use bupha_core::TunnelCfg;
use std::io;
use std::process::{Child, Command};

/// How the webhook listener reaches the public internet.
pub trait Tunnel: Send + Sync {
    fn name(&self) -> &'static str;
    /// Spawn the tunnel process (or `None` for no-op providers like
    /// `direct`, where the box is already public).
    fn spawn(&self) -> io::Result<Option<Child>>;
    /// Public URL to register as the webhook endpoint, if known
    /// statically (named tunnels, direct). `None` = URL appears at
    /// runtime (quick tunnels) — read it from the provider's output.
    fn public_url(&self) -> Option<String> {
        None
    }
}

/// `cloudflared` provider — named tunnel (`name` set): stable URL
/// from ~/.cloudflared/config.yml, survives restarts — webhook
/// registered once. Empty `name`: quick tunnel, fresh
/// *.trycloudflare.com URL per start — re-register the webhook after
/// restarts (see deploy/register-webhook.sh).
pub struct CloudflaredTunnel {
    cfg: TunnelCfg,
}

impl CloudflaredTunnel {
    pub fn new(cfg: TunnelCfg) -> Self {
        Self { cfg }
    }
}

impl Tunnel for CloudflaredTunnel {
    fn name(&self) -> &'static str {
        "cloudflared"
    }

    fn spawn(&self) -> io::Result<Option<Child>> {
        let mut cmd = Command::new("cloudflared");
        cmd.arg("tunnel");
        if self.cfg.name.is_empty() {
            cmd.args(["--url", &self.cfg.local_url, "--no-autoupdate"]);
        } else {
            cmd.args(["run", &self.cfg.name]);
        }
        cmd.spawn().map(Some)
    }

    fn public_url(&self) -> Option<String> {
        (!self.cfg.public_url.is_empty()).then(|| self.cfg.public_url.clone())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// Port default (ex-`bupha-tunnel` test): providers whose URL
    /// only exists at runtime return None from `public_url`.
    struct Direct;

    impl Tunnel for Direct {
        fn name(&self) -> &'static str {
            "direct"
        }
        fn spawn(&self) -> io::Result<Option<Child>> {
            Ok(None)
        }
    }

    #[test]
    fn default_public_url_is_none() {
        let t = Direct;
        assert_eq!(t.name(), "direct");
        assert!(t.public_url().is_none());
        assert!(t.spawn().unwrap().is_none());
    }
}
