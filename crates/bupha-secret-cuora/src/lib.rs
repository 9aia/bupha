//! Cuora impl of `Secrets` — the canonical secrets story (T-056).
//! Thin wrapper over the shared [`hypha_secret_cuora`] adapter,
//! pinned to the `bupha` scope.
//!
//! Each bupha secret NAME resolves to a fixed cuora ref:
//! `cuora://bupha/<NAME>` — e.g. `BUPHA_WH_9AIA_BUPHA` maps to
//! `cuora://bupha/BUPHA_WH_9AIA_BUPHA`. Values are fetched through
//! cuora so audit events, guardrails, and vault policy apply
//! exactly as for every other consumer; bupha never stores a value
//! itself.
//!
//! Two transports, selected by `BUPHA_CUORA_TRANSPORT` (the
//! `<PRODUCT>_CUORA_TRANSPORT` convention — see `hypha-secret-cuora`):
//!
//! - `cli` (default) — shell `cuora get`/`put` per call. Secret
//!   resolution happens a handful of times per run/verb, so a
//!   per-call process is cheap and needs no shared mutable state.
//! - `serve` — a persistent `cuora serve` child speaking the
//!   ndjson JSON-RPC wire (`cuora-serve/1`) on stdio. Spawned lazily
//!   on first use, reused for the client's lifetime.
//!
//! A missing `cuora` binary, a denied op, or an absent ref all
//! answer `None` — "not here" — so the caller's chain falls through
//! to the legacy stores exactly like a keyring miss on a headless
//! box, on either transport.
//!
//! Env knobs: `BUPHA_CUORA_BIN` overrides the binary path (tests
//! stub it); `BUPHA_CUORA_TRANSPORT` picks `cli`/`serve`;
//! `CUORA_*` and `--config` resolution stay cuora's own — the child
//! inherits the environment, so `CUORA_ENV` picks the vault env —
//! bupha does not pin one.

pub use hypha_secret_cuora::{CuoraServe, CuoraTransport};

use hypha_secret::Secrets;
use std::path::PathBuf;
use std::sync::OnceLock;

/// Ref namespace — every bupha secret name lives under
/// `cuora://bupha/…` so `cuora list` shows the whole fleet surface.
pub const NAMESPACE: &str = "bupha";

/// The env knobs this shim reads — re-exported for consumers that
/// build their own transport legs.
pub const BIN_ENV: &str = "BUPHA_CUORA_BIN";
pub const TRANSPORT_ENV: &str = "BUPHA_CUORA_TRANSPORT";

/// `Secrets` over cuora — CLI or `cuora serve` transport per
/// [`CuoraTransport`]. Thin wrapper over
/// [`hypha_secret_cuora::CuoraSecrets`] pinned to the `bupha`
/// scope; the env knobs stay bupha's.
///
/// Clones share the serve child — see
/// [`CuoraSecrets::shared_default`] for why that matters.
#[derive(Clone)]
pub struct CuoraSecrets {
    /// The `cuora` binary — `BUPHA_CUORA_BIN` when set, else `cuora`
    /// resolved on PATH. Kept as a field (not just inside `inner`)
    /// because consumers document it as the test seam.
    pub bin: PathBuf,
    inner: hypha_secret_cuora::CuoraSecrets,
}

impl Default for CuoraSecrets {
    fn default() -> Self {
        Self::from_env()
    }
}

impl std::fmt::Debug for CuoraSecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CuoraSecrets")
            .field("bin", &self.bin)
            .field("transport", &self.inner.transport())
            .finish_non_exhaustive()
    }
}

impl CuoraSecrets {
    /// The env-selected store — `BUPHA_CUORA_BIN` +
    /// `BUPHA_CUORA_TRANSPORT`.
    fn from_env() -> Self {
        Self::with_transport(
            std::env::var_os(BIN_ENV).map_or_else(|| PathBuf::from("cuora"), PathBuf::from),
            CuoraTransport::from_env(TRANSPORT_ENV),
        )
    }

    /// A store over `bin` with the env-selected transport.
    #[must_use]
    pub fn with_bin(bin: PathBuf) -> Self {
        Self::with_transport(bin, CuoraTransport::from_env(TRANSPORT_ENV))
    }

    /// A store over `bin` on an explicit transport — the test seam
    /// and the "I know what I'm doing" constructor.
    #[must_use]
    pub fn with_transport(bin: PathBuf, transport: CuoraTransport) -> Self {
        Self {
            bin: bin.clone(),
            inner: hypha_secret_cuora::CuoraSecrets::new(NAMESPACE)
                .with_bin(bin)
                .with_transport(transport),
        }
    }

    /// The process-wide default store.
    ///
    /// Worth sharing under the serve transport: `CuoraSecrets` is
    /// cheap to construct but the serve child's whole point is
    /// persistence — a fresh store per lookup would spawn and reap
    /// `cuora serve` on every secret read, worse than the CLI. Under
    /// `cli` sharing is a no-op behaviorally.
    #[must_use]
    pub fn shared_default() -> &'static Self {
        static DEFAULT: OnceLock<CuoraSecrets> = OnceLock::new();
        DEFAULT.get_or_init(CuoraSecrets::from_env)
    }
}

/// Build the ref for a secret name. A free fn so tests pin the
/// mapping without constructing the store.
///
/// `BUPHA_WH_9AIA_BUPHA` → `cuora://bupha/BUPHA_WH_9AIA_BUPHA`.
#[must_use]
pub fn rref(name: &str) -> String {
    hypha_secret_cuora::rref(NAMESPACE, name)
}

impl Secrets for CuoraSecrets {
    fn get(&self, name: &str) -> Option<String> {
        self.inner.get(name)
    }

    /// Deploy-time migration — `cuora put` reads the value on stdin
    /// (never argv) on the CLI leg; under serve it rides the
    /// request's `value` field, still never argv.
    fn set(&self, name: &str, value: &str) -> std::io::Result<()> {
        self.inner.set(name, value)
    }
}
