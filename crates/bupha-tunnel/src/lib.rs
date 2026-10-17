//! bupha-tunnel — the tunnel *port*: make the local webhook listener
//! reachable. Implementations live in `bupha-tunnel-*` crates
//! (cloudflared, bore, tailscale, direct/no-op…).

use std::io;
use std::process::Child;

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
