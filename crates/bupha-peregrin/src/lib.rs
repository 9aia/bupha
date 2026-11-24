//! bupha-peregrin — the ci-job integration adapter: everything a
//! peregrin node/hub needs to host `kind="ci-job"` entities, owned
//! here so peregrin stays CI-agnostic (P-047, mirroring the
//! `sepia-peregrin` pattern — the provider repo ships the adapter,
//! the orchestrator composes it).
//!
//! - [`ci_job_plane`] — build the [`bupha_plane::CiJobPlane`] with a
//!   node's capability labels + the resolved fleet specs; register
//!   the result on the node's kind registry like any other plane.
//! - [`fleet_specs`] — resolve every `WatchedRepo` spec visible to
//!   the host (`PEREGRIN_BUPHA_CONFIG`/`BUPHA_CONFIG`, then
//!   `~/.config/bupha/bundles/*.{toml,yaml,yml}`) for the plane's
//!   indexDoc normalization.
//! - [`rehydrate`] — rebuild a restarted node's plane from the hub's
//!   durable entity index before the listener binds.
//! - [`CiJobKindPolicy`] — the hub-side [`hypha_sync::kind::KindPolicy`]
//!   for `ci.*` ops: deliberately *not* lock-gated (the plane's own
//!   CAS is the mutual exclusion — gating `ci.renew`/`ci.complete`
//!   would deadlock the claim holder against itself); `ci.enqueue`/
//!   `ci.cancel` retry as [`OpKind::Metadata`], the CAS/fence-guarded
//!   ops [`OpKind::Turn`].
//!
//! # The `deliveryId` wire contract
//!
//! Forge ingress normalizes deliveries into `ci-job` indexDocs; the
//! audit/dedup field is `deliveryId`:
//!
//! 1. The GitHub webhook ingress (`X-GitHub-Delivery` header) emits
//!    `delivery_id` in every `Event.payload` — empty string when the
//!    header is absent, so `${event.payload.delivery_id}` templates
//!    resolve deterministically.
//! 2. The ci-ingress campaign maps that payload field onto the
//!    indexDoc's `deliveryId`.
//! 3. [`bupha_plane::CiJob`] normalization drops empty strings to
//!    absent — `deliveryId` is absent-or-real on the wire.
//!
//! The webhook driver's `delivery_id` emission is the producer half
//! of this contract; this crate is the consumer half. Change one
//! side and the other's templates/dedup silently break — keep them
//! in lockstep.

mod fleet;
mod plane;
mod policy;
mod rehydrate;

pub use fleet::fleet_specs;
pub use plane::ci_job_plane;
pub use policy::{ci_job_kind_policies, CiJobKindPolicy};
pub use rehydrate::rehydrate;
