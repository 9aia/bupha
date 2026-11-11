//! The `kind="ci-job"` entity model — the indexDoc shape, validation,
//! and deterministic id grammar pinned by P-002
//! (`project/proposals/proposal-0002-*.md`).
//!
//! Pure data + checks: [`CiJob`] is the stored spec, [`JobState`] the
//! state machine, and [`CiJob::from_index_doc`] the create-time trust
//! boundary (the wire is hostile — `is_valid_sha`/`is_valid_key` and
//! the closed sets are re-checked here exactly like
//! `MemWorkQueue::enqueue` re-checks hostile shas). All state
//! transitions and lane math live in [`crate::CiJobPlane`].

use bupha_core::{is_valid_key, is_valid_sha};
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The entity kind tag this plane serves.
pub const CI_JOB_KIND: &str = "ci-job";

/// `ci-job-snapshot/v1` — the export/import payload `format` tag.
pub const SNAPSHOT_FORMAT: &str = "ci-job-snapshot/v1";

/// Default claim lease — carries over the watcher's 5-minute cadence.
pub const DEFAULT_LEASE_SECS: u64 = 300;

/// `repo.retry ≤ 3` ⇒ `maxAttempts ≤ 4` (P-002 §1: the plane re-checks
/// the payload because the wire is a trust boundary).
pub const MAX_ATTEMPTS: u32 = 4;

/// Terminal + live states of a ci-job entity (P-002 §1 state machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum JobState {
    #[default]
    Pending,
    Running,
    Success,
    Failure,
    Cancelled,
    Error,
    /// Superseded by a newer enqueue (latest sweep) or evicted off a
    /// full `all` lane — never retried.
    Superseded,
}

impl JobState {
    /// Wire spelling (`"pending"`, `"running"`, …).
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
            Self::Superseded => "superseded",
        }
    }

    /// Nothing left to do — no claim, no retry, no transition out.
    #[must_use]
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Success | Self::Failure | Self::Cancelled | Self::Error | Self::Superseded
        )
    }

    /// `ci.complete`'s `result` payload → terminal state. `superseded`
    /// is not a worker-reported result.
    #[must_use]
    pub fn from_result(result: &str) -> Option<Self> {
        match result {
            "success" => Some(Self::Success),
            "failure" => Some(Self::Failure),
            "cancelled" => Some(Self::Cancelled),
            "error" => Some(Self::Error),
            _ => None,
        }
    }
}

fn default_event() -> String {
    "push".into()
}

fn default_queue() -> String {
    "latest".into()
}

fn default_one() -> u32 {
    1
}

/// One ci-job entity — the `indexDoc` of `kind="ci-job"` (P-002 §1).
/// Serialized camelCase on the wire; `id` is derived, never trusted
/// from the payload (`#[serde(skip)]` both ways).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CiJob {
    /// `ci:{key}:{sha}:{attempt}` — deterministic, so webhook replays
    /// dedup for free (create is idempotent on this id).
    #[serde(skip)]
    pub id: String,
    /// `WatchedRepo.key` — `owner/name[:job]`; the queue lane id.
    pub key: String,
    /// Status/forge repo — `status_repo`, defaults to the key's repo
    /// part when absent.
    #[serde(default)]
    pub repo: String,
    /// `Push.git_ref` — wire name `ref`.
    #[serde(rename = "ref", default)]
    pub git_ref: String,
    /// `Push.sha` — `is_valid_sha`'d at the trust boundary.
    pub sha: String,
    /// `Push.event` — closed set `"push"|"pull_request"`.
    #[serde(default = "default_event")]
    pub event: String,
    /// `Push.sender` — forge-attested.
    #[serde(default)]
    pub sender: String,
    /// Forge delivery id — audit/dedup only (the entity id already
    /// dedups replays).
    #[serde(default)]
    pub delivery_id: Option<String>,
    /// Resolved queue policy — closed set `"latest"|"all"`.
    #[serde(default = "default_queue")]
    pub queue: String,
    /// Resolved label requirements (job → repo → []); a claiming
    /// worker's advertised capabilities must cover them.
    #[serde(default)]
    pub runs_on: Vec<String>,
    /// Resolved runner config verbatim — `token_env` is a NAME, safe
    /// to store. `cancel_running` inside it conflicts with `all`.
    #[serde(default)]
    pub runner: Value,
    /// Status context label — defaults to `ci/bupha[/<job>]`.
    #[serde(default)]
    pub context: String,
    /// `repo.retry + 1` resolved at ingest; `1..=MAX_ATTEMPTS`.
    #[serde(default = "default_one")]
    pub max_attempts: u32,
    /// 1-based attempt — auto-retry spawns a NEW entity at +1.
    #[serde(default = "default_one")]
    pub attempt: u32,
    #[serde(default)]
    pub state: JobState,
    /// RFC3339 — when the job (re-)entered the pending lane.
    #[serde(default)]
    pub enqueued_at: String,
    /// Worker id (`<node>/<pid>`) once running.
    #[serde(default)]
    pub claimed_by: Option<String>,
    /// Fencing token — bumped on every claim; renew/release/complete
    /// must present it.
    #[serde(default)]
    pub fence: u64,
    /// RFC3339 lease expiry — lazy-swept back to `pending`.
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    /// Sha that superseded this job (latest sweep / `all` eviction).
    #[serde(default)]
    pub superseded_by: Option<String>,
    /// `ci.cancel` on a running job — the worker's renew loop polls
    /// and kills its child.
    #[serde(default)]
    pub cancel_requested: bool,
    /// Terminal outcome — `success|failure|cancelled|error|
    /// superseded`.
    #[serde(default)]
    pub result: Option<String>,
    /// Short outcome text (status description).
    #[serde(default)]
    pub description: Option<String>,
    /// Optional pointer to run logs/artifacts.
    #[serde(default)]
    pub log_ref: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub finished_at: Option<String>,
    /// Plane bookkeeping — not part of the P-002 indexDoc contract,
    /// skipped on serialize so `indexDoc` matches the spec exactly.
    #[serde(skip_serializing, default)]
    pub created_at: String,
    #[serde(skip_serializing, default)]
    pub updated_at: String,
}

/// `owner/name` from a `key` — the part before the first `:` job
/// separator (`o/n` stays `o/n`).
fn repo_part(key: &str) -> &str {
    key.split(':').next().unwrap_or(key)
}

/// Job name from a `key` — the part after the first `:`.
fn job_part(key: &str) -> Option<&str> {
    key.split_once(':').map(|(_, job)| job)
}

impl CiJob {
    /// The deterministic entity id — `ci:{key}:{sha}:{attempt}`.
    #[must_use]
    pub fn compute_id(&self) -> String {
        format!("ci:{}:{}:{}", self.key, self.sha, self.attempt)
    }

    /// Parse + validate an `indexDoc` payload (the create trust
    /// boundary). `now` is the RFC3339 stamp for empty bookkeeping
    /// fields. Fails with a human-readable reason on any violation.
    ///
    /// # Errors
    /// Returns `Err` when the payload isn't a ci-job doc or fails
    /// validation (bad sha/key, closed-set violations, `retry`
    /// bound, `all`+`cancel_running` conflict).
    pub fn from_index_doc(value: &Value, now: &str) -> Result<Self, String> {
        let mut job: CiJob = serde_json::from_value(value.clone())
            .map_err(|e| format!("invalid ci-job indexDoc: {e}"))?;
        job.validate()?;
        if job.repo.is_empty() {
            job.repo = repo_part(&job.key).to_string();
        }
        if job.context.is_empty() {
            job.context = match job_part(&job.key) {
                Some(job_name) => format!("ci/bupha/{job_name}"),
                None => "ci/bupha".to_string(),
            };
        }
        // An empty delivery id carries no information — store None
        // so `deliveryId` stays absent-or-real on the wire.
        if job.delivery_id.as_deref() == Some("") {
            job.delivery_id = None;
        }
        if job.enqueued_at.is_empty() {
            job.enqueued_at = now.to_string();
        }
        if job.created_at.is_empty() {
            job.created_at = now.to_string();
        }
        if job.updated_at.is_empty() {
            job.updated_at = now.to_string();
        }
        job.id = job.compute_id();
        Ok(job)
    }

    /// The wire `indexDoc` — the stored spec verbatim (P-002 §1).
    #[must_use]
    pub fn to_index_doc(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }

    /// Create-time validation — mirrors `Config::watched`'s checks on
    /// the resolved spec, re-applied because the wire is hostile.
    /// `pub(crate)`: the plane re-runs it after fleet normalization
    /// overwrites the resolved fields.
    pub(crate) fn validate(&self) -> Result<(), String> {
        if !is_valid_key(&self.key) {
            return Err(format!("invalid job key '{}'", self.key));
        }
        if !is_valid_sha(&self.sha) {
            return Err(format!("invalid sha '{}'", self.sha));
        }
        if !matches!(self.queue.as_str(), "latest" | "all") {
            return Err(format!(
                "queue must be \"latest\" or \"all\", got '{}'",
                self.queue
            ));
        }
        if !matches!(self.event.as_str(), "push" | "pull_request") {
            return Err(format!(
                "event must be \"push\" or \"pull_request\", got '{}'",
                self.event
            ));
        }
        if self.attempt == 0 {
            return Err("attempt must be >= 1".to_string());
        }
        if !(1..=MAX_ATTEMPTS).contains(&self.max_attempts) {
            return Err(format!(
                "maxAttempts must be 1..={MAX_ATTEMPTS} (retry <= 3), got {}",
                self.max_attempts
            ));
        }
        // `queue = "all"` + `runner.cancel_running` — the same conflict
        // `Config::watched` rejects at load (`bupha-core`). The runner
        // doc arrives as opaque JSON; accept either spelling.
        if self.queue == "all"
            && (self.runner.get("cancel_running").and_then(Value::as_bool) == Some(true)
                || self.runner.get("cancelRunning").and_then(Value::as_bool) == Some(true))
        {
            return Err(
                "queue \"all\" conflicts with runner.cancel_running — every push runs, \
                 nothing in flight may be killed"
                    .to_string(),
            );
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn doc() -> Value {
        json!({
            "key": "owner/name:gate",
            "sha": "0123abcd4567",
            "repo": "owner/name",
            "ref": "refs/heads/main",
            "event": "push",
            "sender": "luis",
            "deliveryId": "guid-1",
            "queue": "latest",
            "runsOn": ["arch=arm64"],
            "runner": {"kind": "shell"},
            "context": "ci/bupha/gate",
            "maxAttempts": 3,
            "attempt": 1,
            "state": "pending"
        })
    }

    #[test]
    fn parses_full_doc_and_derives_id() {
        let job = CiJob::from_index_doc(&doc(), "2026-01-01T00:00:00Z").unwrap();
        assert_eq!(job.id, "ci:owner/name:gate:0123abcd4567:1");
        assert_eq!(job.key, "owner/name:gate");
        assert_eq!(job.state, JobState::Pending);
        assert_eq!(job.max_attempts, 3);
        assert_eq!(job.enqueued_at, "2026-01-01T00:00:00Z");
        assert_eq!(job.created_at, "2026-01-01T00:00:00Z");
    }

    #[test]
    fn minimal_doc_gets_derived_defaults() {
        let job = CiJob::from_index_doc(
            &json!({"key": "o/n:j", "sha": "abc1234"}),
            "2026-01-01T00:00:00Z",
        )
        .unwrap();
        assert_eq!(job.repo, "o/n");
        assert_eq!(job.context, "ci/bupha/j");
        assert_eq!(job.queue, "latest");
        assert_eq!(job.event, "push");
        assert_eq!(job.max_attempts, 1);
        assert_eq!(job.attempt, 1);
    }

    #[test]
    fn rejects_hostile_payloads() {
        for (patch, want) in [
            (json!({"sha": "-oops"}), "invalid sha"),
            (json!({"key": "a/../b"}), "invalid job key"),
            (json!({"queue": "sometimes"}), "queue must be"),
            (json!({"event": "webhook"}), "event must be"),
            (json!({"maxAttempts": 5}), "maxAttempts"),
            (json!({"attempt": 0}), "attempt"),
        ] {
            let mut d = doc();
            let obj = d.as_object_mut().unwrap();
            for (k, v) in patch.as_object().unwrap() {
                obj.insert(k.clone(), v.clone());
            }
            let err = CiJob::from_index_doc(&d, "2026-01-01T00:00:00Z").unwrap_err();
            assert!(err.contains(want), "{err} should contain {want}");
        }
        // missing required fields
        assert!(CiJob::from_index_doc(&json!({"sha": "abc1234"}), "t").is_err());
        assert!(CiJob::from_index_doc(&json!({"key": "o/n"}), "t").is_err());
    }

    #[test]
    fn all_plus_cancel_running_conflicts() {
        let mut d = doc();
        d["queue"] = json!("all");
        d["runner"] = json!({"kind": "shell", "cancel_running": true});
        let err = CiJob::from_index_doc(&d, "t").unwrap_err();
        assert!(err.contains("cancel_running"), "{err}");
        // camelCase spelling conflicts too
        d["runner"] = json!({"cancelRunning": true});
        assert!(CiJob::from_index_doc(&d, "t").is_err());
        // ...but `latest` + cancel_running is fine
        d["queue"] = json!("latest");
        assert!(CiJob::from_index_doc(&d, "t").is_ok());
    }

    #[test]
    fn index_doc_matches_p002_shape() {
        let job = CiJob::from_index_doc(&doc(), "2026-01-01T00:00:00Z").unwrap();
        let v = job.to_index_doc();
        for field in [
            "key",
            "repo",
            "ref",
            "sha",
            "event",
            "sender",
            "deliveryId",
            "queue",
            "runsOn",
            "runner",
            "context",
            "maxAttempts",
            "attempt",
            "state",
            "enqueuedAt",
            "claimedBy",
            "fence",
            "leaseExpiresAt",
            "supersededBy",
            "cancelRequested",
            "result",
            "description",
            "logRef",
            "startedAt",
            "finishedAt",
        ] {
            assert!(v.get(field).is_some(), "indexDoc missing '{field}'");
        }
        // bookkeeping fields stay out of the contract surface
        assert!(v.get("createdAt").is_none());
        assert!(v.get("updatedAt").is_none());
        assert!(v.get("id").is_none());
    }

    #[test]
    fn state_helpers() {
        assert!(!JobState::Pending.is_terminal());
        assert!(!JobState::Running.is_terminal());
        for s in [
            JobState::Success,
            JobState::Failure,
            JobState::Cancelled,
            JobState::Error,
            JobState::Superseded,
        ] {
            assert!(s.is_terminal());
        }
        assert_eq!(JobState::from_result("failure"), Some(JobState::Failure));
        assert_eq!(JobState::from_result("superseded"), None);
        assert_eq!(JobState::from_result("bogus"), None);
    }
}
