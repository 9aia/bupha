//! bupha-tunnel-cloudflared — `cloudflared` provider.
//!
//! Named tunnel (`name` set): stable URL from ~/.cloudflared/config.yml,
//! survives restarts — webhook registered once. Empty `name`: quick
//! tunnel, fresh *.trycloudflare.com URL per start — re-register the
//! webhook after restarts (see deploy/register-webhook.sh).

use bupha_core::TunnelCfg;
use bupha_tunnel::Tunnel;
use std::io;
use std::process::{Child, Command};

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
