//! Embedded surface dispatch — the driver serving its own engine's
//! ops through `pavo-driver-kit`'s surface layer, no second binary
//! needed. `cli`/`mcp` argv verbs (feature-gated) plug this
//! [`Surface`] impl into `surface::cli::run` / `surface::mcp::serve_stdio`.
//!
//! `op` reuses the same `cli.*` path wire queries take
//! (`engine_ops::run_verb` re-execs `run` in a captured child —
//! output stays off the serve stream). `surface` dispatches
//! `watch`/`agent`/`gha-runner` in-process: those verbs are
//! unbounded by design, so argv goes straight to `bupha::run_from`
//! with stdio streaming.

use pavo_driver_kit::surface::{OpOutcome, OpSpec, Surface};
use serde_json::{Map, Value};

use crate::engine_ops;

/// bupha's [`Surface`] — engine dispatch, already linked.
pub struct Engine;

impl Surface for Engine {
    /// Same path as the `cli.<op>` wire query: re-exec `run` in a
    /// captured child, output → `OpOutcome`.
    fn op(&mut self, op: &OpSpec, params: &Map<String, Value>) -> OpOutcome {
        OpOutcome::from_response(&engine_ops::run_op(&op.name, params))
    }

    /// `bupha <name> <args>` in-process — unbounded surfaces stream
    /// until they stop themselves (signals), which is exactly why
    /// they're surfaces and not ops.
    fn surface(&mut self, name: &str, args: &[String]) -> i32 {
        let argv = std::iter::once("bupha".to_string())
            .chain(std::iter::once(name.to_string()))
            .chain(args.iter().cloned());
        bupha::run_from(argv)
    }
}
