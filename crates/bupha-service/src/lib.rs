//! Service port — OS-level service lifecycle.
//! systemd impl lives in `bupha-deploy`; launchd/Windows = new impls.

use std::io;
use std::path::PathBuf;

/// Install/remove a managed service so `watch` survives reboots.
pub trait Service: Send + Sync {
    /// Write the unit/config file; returns where it landed.
    fn write(&self) -> io::Result<PathBuf>;
    /// Enable + start now.
    fn enable(&self) -> io::Result<()>;
    /// Disable + stop + remove the file.
    fn disable(&self) -> io::Result<PathBuf>;
}
