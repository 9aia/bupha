//! Wire types for the Actions runner protocol — the shapes the
//! broker, the run service and the results service exchange.
//!
//! Two JSON dialects appear on the wire and are *not* handled here:
//! `data`-encoded contexts and `token`-encoded step fields are
//! unwrapped by [`crate::encoding`] before these types see them.
//! Field names follow the service: camelCase almost everywhere, with
//! the run-service endpoints (`acquirejob`/`completejob`/`renewjob`)
//! spelled the same way.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

/// Version we claim. Third-party runners have no binaries to swap,
/// so we report a plausible version and set `disable_update`.
pub const RUNNER_VERSION: &str = "3.0.0";

// ------------------------------------------------------------------
// registration + session
// ------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TenantRequest<'a> {
    pub url: &'a str,
    pub runner_event: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Tenant {
    pub url: String,
    pub token: String,
    #[serde(default)]
    pub token_schema: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Pools {
    #[serde(default)]
    pub value: Vec<Pool>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pool {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub is_hosted: bool,
    #[serde(default)]
    pub pool_type: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRequest {
    pub name: String,
    pub version: &'static str,
    pub os_description: String,
    pub max_parallelism: u32,
    pub ephemeral: bool,
    pub disable_update: bool,
    pub provisioning_state: &'static str,
    pub labels: Vec<Label>,
    pub authorization: PublicKeyRequest,
}

#[derive(Debug, Clone, Serialize)]
pub struct Label {
    pub name: String,
    /// `system` for the labels every runner has, `user` for the rest.
    #[serde(rename = "type")]
    pub kind: &'static str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicKeyRequest {
    pub public_key: PublicKey,
}

/// Base64 of big-endian bytes — how the service spells a key.
#[derive(Debug, Clone, Serialize)]
pub struct PublicKey {
    pub exponent: String,
    pub modulus: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Agent {
    pub id: i64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub authorization: AgentAuthorization,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AgentAuthorization {
    pub client_id: String,
    pub authorization_url: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionRequest<'a> {
    pub agent: AgentReference<'a>,
    pub owner_name: &'a str,
}

#[derive(Debug, Clone, Serialize)]
pub struct AgentReference<'a> {
    pub id: i64,
    pub name: &'a str,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_id: String,
    #[serde(default)]
    pub encryption_key: Option<EncryptionKey>,
    #[serde(default)]
    pub use_fips_encryption: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EncryptionKey {
    /// Base64; RSA-OAEP wrapped to the runner key when `encrypted`.
    pub value: String,
    #[serde(default)]
    pub encrypted: bool,
}

/// The broker keeps its own session; the agent's public key goes
/// along so the broker can wrap the session key to it.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerSessionRequest<'a> {
    pub agent: BrokerAgent<'a>,
    pub owner_name: &'a str,
    pub use_fips_encryption: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BrokerAgent<'a> {
    pub id: i64,
    pub name: &'a str,
    pub version: &'static str,
    pub authorization: PublicKeyRequest,
}

// ------------------------------------------------------------------
// messages
// ------------------------------------------------------------------

/// One message off the queue. `body` is plain JSON for jobs we can
/// read; `iv` accompanies AES-encrypted bodies when the session
/// carries a key.
#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Envelope {
    #[serde(default)]
    pub message_id: Option<i64>,
    pub message_type: String,
    pub body: String,
    #[serde(default)]
    pub iv: Option<String>,
}

impl Envelope {
    /// The broker hands over a *pointer* to the job rather than the
    /// job itself (the modern flow).
    pub fn job_offer(&self) -> Option<JobOffer> {
        if !self.message_type.eq_ignore_ascii_case("RunnerJobRequest") {
            return None;
        }
        serde_json::from_str(&self.body).ok()
    }

    /// The service hands the runner over to the broker once, in a
    /// message with no id of its own.
    pub fn broker_url(&self) -> Option<String> {
        if !self.message_type.eq_ignore_ascii_case("BrokerMigration") {
            return None;
        }
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Migration {
            broker_base_url: String,
        }
        serde_json::from_str::<Migration>(&self.body)
            .ok()
            .map(|m| m.broker_base_url)
    }

    /// Legacy flow: the job arrives inline in the message body.
    pub fn is_inline_job(&self) -> bool {
        self.message_type
            .eq_ignore_ascii_case("PipelineAgentJobRequest")
    }

    /// Cancellation arrives as a message type, not an HTTP call.
    pub fn is_cancel(&self) -> bool {
        self.message_type.to_ascii_lowercase().contains("cancel")
    }

    /// Update nags a third-party runner cannot honour — we registered
    /// with `disable_update`, so these are noise.
    pub fn is_update(&self) -> bool {
        let t = self.message_type.to_ascii_lowercase();
        t.contains("refresh") || t.contains("update")
    }
}

/// Where to collect a job the broker offered. The service has spelled
/// these both ways across API versions — aliases cover both.
#[derive(Debug, Clone, Deserialize)]
pub struct JobOffer {
    #[serde(alias = "runServiceUrl")]
    pub run_service_url: String,
    #[serde(alias = "runnerRequestId")]
    pub runner_request_id: String,
    #[serde(default, alias = "billingOwnerId")]
    pub billing_owner_id: String,
    /// Whether the offer message still needs acknowledging.
    #[serde(default, alias = "shouldAcknowledge")]
    pub should_acknowledge: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AcquireJobRequest<'a> {
    pub job_message_id: &'a str,
    #[serde(rename = "runnerOS")]
    pub runner_os: &'a str,
    pub billing_owner_id: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenewJobRequest<'a> {
    pub plan_id: &'a str,
    pub job_id: &'a str,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CompleteJobRequest<'a> {
    pub plan_id: &'a str,
    pub job_id: &'a str,
    /// `succeeded`, `failed` or `canceled` — the service's spellings.
    pub conclusion: &'a str,
    pub billing_owner_id: &'a str,
    /// Evaluated job `outputs:` — the channel `needs.*.outputs`
    /// consumers read back on later jobs. Omitted when empty (the
    /// service treats absent and empty the same).
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub outputs: &'a BTreeMap<String, Variable>,
    #[serde(rename = "stepResults")]
    pub step_results: &'a [StepResult],
}

// ------------------------------------------------------------------
// the job itself
// ------------------------------------------------------------------

/// A job already expanded server-side: matrix resolved, `needs`
/// outputs filled, steps flat. Unknown top-level fields are ignored —
/// the service adds fields faster than this crate tracks them, and a
/// job we can run correctly is better than one we refuse.
#[derive(Debug, Clone)]
pub struct JobMessage {
    pub message_type: String,
    pub job_id: String,
    pub job_display_name: String,
    pub job_name: String,
    /// The request id the lease is renewed against.
    pub request_id: i64,
    pub billing_owner_id: String,
    pub plan_id: String,
    pub timeline_id: String,
    pub steps: Vec<PipelineStep>,
    /// `contextData` after data-decoding — `github`, `job`,
    /// `strategy`, `inputs`, `matrix`, `needs`, `vars` live under it.
    pub context: Value,
    /// Feature flags and tokens keyed by dotted name.
    pub variables: BTreeMap<String, Variable>,
    /// `resources.endpoints` — the run/results/feed URLs and the
    /// job-scoped access token live here.
    pub resources: Value,
    /// Job-level environment after data-decoding (may be empty).
    pub environment_variables: BTreeMap<String, String>,
    /// The job's `outputs:` mapping — output name → value template
    /// (`${{ steps.<id>.outputs.<name> }}` source form), token-
    /// decoded like step fields. The runner evaluates these against
    /// the collected step outputs and reports the result on
    /// `completejob`; the service then fills `needs.*.outputs` for
    /// dependent jobs.
    pub job_outputs: BTreeMap<String, String>,
    pub locked_until: Value,
}

impl JobMessage {
    /// Parse the job JSON the acquire/inline path returns. Step fields
    /// arrive token-encoded and contexts data-encoded; both are
    /// unwrapped here so callers see plain values.
    pub fn decode(raw: &str) -> Result<Self, String> {
        let value: Value =
            serde_json::from_str(raw).map_err(|e| format!("job message is not JSON: {e}"))?;
        let obj = value
            .as_object()
            .ok_or_else(|| "job message is not an object".to_string())?;
        Ok(Self {
            message_type: str_field(obj, "messageType"),
            job_id: str_field(obj, "jobId"),
            job_display_name: str_field(obj, "jobDisplayName"),
            job_name: str_field(obj, "jobName"),
            request_id: obj
                .get("requestId")
                .and_then(Value::as_i64)
                .unwrap_or_default(),
            billing_owner_id: str_field(obj, "billingOwnerId"),
            plan_id: obj
                .get("plan")
                .and_then(|p| p.get("planId"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            timeline_id: obj
                .get("timeline")
                .and_then(|t| t.get("id"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            steps: decode_steps(obj.get("steps"))?,
            context: crate::encoding::read_data(
                obj.get("contextData").cloned().unwrap_or(Value::Null),
            ),
            variables: decode_variables(obj.get("variables")),
            resources: obj.get("resources").cloned().unwrap_or(Value::Null),
            environment_variables: decode_env_map(obj.get("environmentVariables")),
            job_outputs: decode_job_outputs(obj.get("jobOutputs")),
            locked_until: obj.get("lockedUntil").cloned().unwrap_or(Value::Null),
        })
    }

    /// `env()` for callers that want the variables as strings.
    pub fn variable(&self, name: &str) -> Option<&str> {
        self.variables.get(name).map(|v| v.value.as_str())
    }

    /// The token job-scoped calls authenticate with — from the
    /// endpoint's `authorization.parameters.AccessToken`.
    pub fn service_token(&self) -> Option<&str> {
        self.resources
            .get("endpoints")?
            .as_array()?
            .iter()
            .find_map(|e| e.pointer("/authorization/parameters/AccessToken")?.as_str())
    }

    /// The run service base — `acquirejob`/`renewjob`/`completejob`.
    pub fn run_service_url(&self) -> Option<&str> {
        self.resources
            .get("endpoints")?
            .as_array()?
            .first()?
            .get("url")?
            .as_str()
    }

    /// `endpoint.data.<name>` out of `resources.endpoints`.
    pub fn endpoint_data(&self, name: &str) -> Option<&str> {
        self.resources
            .get("endpoints")?
            .as_array()?
            .iter()
            .filter_map(|e| e.get("data"))
            .find_map(|d| d.get(name)?.as_str())
    }

    pub fn results_url(&self) -> Option<&str> {
        self.endpoint_data("ResultsServiceUrl")
    }

    pub fn feed_url(&self) -> Option<&str> {
        self.endpoint_data("FeedStreamUrl")
    }

    /// Secrets the service marked — values to keep out of logs.
    pub fn secrets(&self) -> Vec<String> {
        self.variables
            .values()
            .filter(|v| v.is_secret && !v.value.is_empty())
            .map(|v| v.value.clone())
            .chain(
                self.environment_variables
                    .values()
                    .filter(|v| !v.is_empty())
                    .cloned(),
            )
            .collect()
    }
}

fn str_field(obj: &serde_json::Map<String, Value>, key: &str) -> String {
    obj.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `jobOutputs` is a compiled template map — token-encoded like
/// step fields, so expressions arrive as `${{ … }}` source text.
/// Decoded values that aren't strings are dropped.
fn decode_job_outputs(raw: Option<&Value>) -> BTreeMap<String, String> {
    let decoded = crate::encoding::read_token(raw.cloned().unwrap_or(Value::Null));
    let mut out = BTreeMap::new();
    if let Value::Object(map) = decoded {
        for (k, v) in map {
            if let Value::String(s) = v {
                out.insert(k, s);
            }
        }
    }
    out
}

fn decode_variables(raw: Option<&Value>) -> BTreeMap<String, Variable> {
    let Some(Value::Object(map)) = raw else {
        return BTreeMap::new();
    };
    map.iter()
        .filter_map(|(k, v)| {
            let value = v.get("value")?.as_str()?.to_string();
            Some((
                k.clone(),
                Variable {
                    value,
                    is_secret: v.get("isSecret").and_then(Value::as_bool).unwrap_or(false),
                },
            ))
        })
        .collect()
}

/// `environmentVariables` arrives data-encoded; a decoded object is
/// name→value, anything else is ignored.
fn decode_env_map(raw: Option<&Value>) -> BTreeMap<String, String> {
    let decoded = crate::encoding::read_data(raw.cloned().unwrap_or(Value::Null));
    let mut out = BTreeMap::new();
    if let Value::Object(map) = decoded {
        for (k, v) in map {
            match v {
                Value::String(s) => {
                    out.insert(k, s);
                }
                Value::Number(n) => {
                    out.insert(k, n.to_string());
                }
                Value::Bool(b) => {
                    out.insert(k, b.to_string());
                }
                _ => {}
            }
        }
    }
    out
}

fn decode_steps(raw: Option<&Value>) -> Result<Vec<PipelineStep>, String> {
    let Some(Value::Array(items)) = raw else {
        return Ok(Vec::new());
    };
    items
        .iter()
        .enumerate()
        .map(|(i, v)| decode_step(v, i))
        .collect()
}

fn decode_step(v: &Value, i: usize) -> Result<PipelineStep, String> {
    let what = format!("steps[{i}]");
    let obj = v
        .as_object()
        .ok_or_else(|| format!("{what}: expected an object"))?;
    let tok = |key: &str| crate::encoding::read_token(obj.get(key).cloned().unwrap_or(Value::Null));

    let display_name = match tok("displayNameToken") {
        Value::String(s) => s,
        Value::Null => String::new(),
        other => other.to_string(),
    };
    let name = str_field(obj, "name");
    let context_name = obj
        .get("contextName")
        .and_then(Value::as_str)
        .map(str::to_string);
    let condition = obj
        .get("condition")
        .and_then(Value::as_str)
        .map(str::to_string);
    let continue_on_error = match tok("continueOnError") {
        Value::Bool(b) => b,
        _ => false,
    };
    let timeout_in_minutes = match tok("timeoutInMinutes") {
        Value::Number(n) => n.as_u64(),
        _ => None,
    };
    let inputs = value_map(tok("inputs"), &format!("{what}.inputs"))?;
    // `env` arrives under `environment` on the wire.
    let env = value_map(tok("environment"), &format!("{what}.env"))?;
    let reference = StepReference::decode(
        obj.get("reference")
            .ok_or_else(|| format!("{what}: missing reference"))?,
    )?;
    Ok(PipelineStep {
        kind: str_field(obj, "type"),
        id: str_field(obj, "id"),
        name,
        display_name,
        context_name,
        condition,
        continue_on_error,
        timeout_in_minutes,
        inputs,
        env,
        reference,
    })
}

fn value_map(v: Value, what: &str) -> Result<BTreeMap<String, String>, String> {
    let mut out = BTreeMap::new();
    match v {
        Value::Null => Ok(out),
        Value::Object(map) => {
            for (k, v) in map {
                match v {
                    Value::String(s) => {
                        out.insert(k, s);
                    }
                    Value::Number(n) => {
                        out.insert(k, n.to_string());
                    }
                    Value::Bool(b) => {
                        out.insert(k, b.to_string());
                    }
                    _ => return Err(format!("{what}.{k}: expected a scalar")),
                }
            }
            Ok(out)
        }
        _ => Err(format!("{what}: expected a map")),
    }
}

/// One compiled step — `type` on the wire is always `action`; what
/// it *does* is in `reference`.
#[derive(Debug, Clone)]
pub struct PipelineStep {
    /// Wire `type` — "action" today.
    pub kind: String,
    /// The id results are reported against.
    pub id: String,
    /// `contextName` — the `__run`/`__actions_checkout` handle.
    pub name: String,
    pub display_name: String,
    pub context_name: Option<String>,
    /// e.g. `success()`; absent means the default.
    pub condition: Option<String>,
    pub continue_on_error: bool,
    pub timeout_in_minutes: Option<u64>,
    /// `run:` steps keep their script under `inputs["script"]`;
    /// action steps keep their `with:` inputs here.
    pub inputs: BTreeMap<String, String>,
    pub env: BTreeMap<String, String>,
    pub reference: StepReference,
}

impl PipelineStep {
    pub fn label(&self) -> &str {
        if self.display_name.is_empty() {
            &self.name
        } else {
            &self.display_name
        }
    }
}

#[derive(Debug, Clone)]
pub enum StepReference {
    /// A `run:` step — the script is `inputs["script"]`.
    Script,
    /// A repository action (`actions/checkout@v4`, …).
    Repository {
        name: String,
        r#ref: String,
        path: Option<String>,
        repository_type: String,
    },
    /// A container image step.
    ContainerRegistry { image: String },
    /// A reference shape this crate does not model — kept raw so the
    /// executor can fail loudly naming it.
    Other { raw: Value },
}

impl StepReference {
    fn decode(v: &Value) -> Result<Self, String> {
        let kind = v.get("type").and_then(Value::as_str).unwrap_or_default();
        match kind {
            "script" => Ok(Self::Script),
            "repository" => Ok(Self::Repository {
                name: v
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                r#ref: v
                    .get("ref")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                path: v.get("path").and_then(Value::as_str).map(str::to_string),
                repository_type: v
                    .get("repositoryType")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            "containerRegistry" => Ok(Self::ContainerRegistry {
                image: v
                    .get("image")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
            }),
            _ => Ok(Self::Other { raw: v.clone() }),
        }
    }
}

// ------------------------------------------------------------------
// results service
// ------------------------------------------------------------------

/// How a job ended, in the service's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
}

impl Outcome {
    pub fn name(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "canceled",
        }
    }

    /// The step-level spelling (`cancelled` has two l's here).
    pub fn step_name(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct StepResult {
    pub external_id: String,
    pub number: u32,
    pub name: String,
    /// `pending`, `inProgress` or `completed`.
    pub status: &'static str,
    /// `succeeded`, `failed`, `skipped`; absent while running.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conclusion: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct StepsUpdateRequest<'a> {
    pub workflow_run_backend_id: &'a str,
    pub workflow_job_run_backend_id: &'a str,
    /// Orders updates that arrive out of order.
    pub change_order: u64,
    pub steps: &'a [StepResult],
}

#[derive(Debug, Clone, Serialize)]
pub struct SignedLogUrlRequest<'a> {
    pub workflow_run_backend_id: &'a str,
    pub workflow_job_run_backend_id: &'a str,
    /// Absent when the log is the job's rather than one step's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_backend_id: Option<&'a str>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SignedLogUrl {
    pub logs_url: String,
    #[serde(default)]
    pub blob_storage_type: String,
    #[serde(default)]
    pub soft_size_limit: Option<Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LogsMetadata<'a> {
    pub workflow_run_backend_id: &'a str,
    pub workflow_job_run_backend_id: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_backend_id: Option<&'a str>,
    pub uploaded_at: String,
    pub line_count: u64,
}

/// RFC3339 with milliseconds, UTC — the service's timestamp shape.
pub fn timestamp(at: std::time::SystemTime) -> String {
    chrono::DateTime::<chrono::Utc>::from(at)
        .format("%Y-%m-%dT%H:%M:%S%.3fZ")
        .to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Variable {
    #[serde(default)]
    pub value: String,
    #[serde(default)]
    pub is_secret: bool,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    /// A trimmed `acquirejob` response: token-encoded steps, data-
    /// encoded contexts, endpoint resources with a job token.
    const JOB: &str = r#"{
        "messageType": "PipelineAgentJobRequest",
        "jobId": "job-1",
        "jobDisplayName": "build",
        "jobName": "build",
        "requestId": 42,
        "billingOwnerId": "owner-1",
        "plan": {"planId": "plan-1"},
        "timeline": {"id": "tl-1"},
        "lockedUntil": "2026-01-01T00:00:00Z",
        "contextData": {"t": 2, "d": [
            {"k": "github", "v": {"t": 2, "d": [
                {"k": "repository", "v": "octocat/hello"},
                {"k": "sha", "v": "abc123"}
            ]}},
            {"k": "job", "v": {"t": 2, "d": [
                {"k": "job_id", "v": "job-1"}
            ]}}
        ]},
        "variables": {
            "system.github.token": {"value": "ghs_secret", "isSecret": true},
            "system.runner.name": {"value": "bupha-test", "isSecret": false}
        },
        "resources": {"endpoints": [{
            "url": "https://run.example",
            "data": {"ResultsServiceUrl": "https://results.example"},
            "authorization": {"parameters": {"AccessToken": "svc-token"}}
        }]},
        "environmentVariables": {"t": 2, "d": [
            {"k": "FOO", "v": "bar"}
        ]},
        "steps": [
            {
                "type": "action",
                "id": "step-1",
                "name": "__run",
                "displayNameToken": {"type": 0, "lit": "Run tests"},
                "condition": "success()",
                "reference": {"type": "script"},
                "inputs": {"type": 2, "map": [
                    {"Key": {"type": 0, "lit": "script"},
                     "Value": {"type": 0, "lit": "echo hi"}},
                    {"Key": {"type": 0, "lit": "shell"},
                     "Value": {"type": 0, "lit": "bash"}}
                ]},
                "environment": {"type": 2, "map": [
                    {"Key": {"type": 0, "lit": "STEP_VAR"},
                     "Value": {"type": 0, "lit": "step-env"}}
                ]}
            },
            {
                "type": "action",
                "id": "step-2",
                "name": "__actions_checkout",
                "displayNameToken": {"type": 0, "lit": "Checkout"},
                "reference": {"type": "repository", "name": "actions/checkout",
                              "ref": "v4", "repositoryType": "GitHub"},
                "inputs": {"type": 2, "map": []}
            }
        ]
    }"#;

    #[test]
    fn job_message_decodes_wire_shape() {
        let job = JobMessage::decode(JOB).unwrap();
        assert_eq!(job.job_id, "job-1");
        assert_eq!(job.plan_id, "plan-1");
        assert_eq!(job.timeline_id, "tl-1");
        assert_eq!(job.request_id, 42);
        assert_eq!(job.context["github"]["repository"], "octocat/hello");
        assert_eq!(job.context["job"]["job_id"], "job-1");
        assert_eq!(job.environment_variables["FOO"], "bar");
        assert_eq!(job.variable("system.runner.name"), Some("bupha-test"));
        assert_eq!(job.service_token(), Some("svc-token"));
        assert_eq!(job.run_service_url(), Some("https://run.example"));
        assert_eq!(job.results_url(), Some("https://results.example"));
        assert!(job.secrets().contains(&"ghs_secret".to_string()));

        assert_eq!(job.steps.len(), 2);
        let run = &job.steps[0];
        assert_eq!(run.id, "step-1");
        assert_eq!(run.label(), "Run tests");
        assert_eq!(run.condition.as_deref(), Some("success()"));
        assert_eq!(run.inputs["script"], "echo hi");
        assert_eq!(run.env["STEP_VAR"], "step-env");
        assert!(matches!(run.reference, StepReference::Script));

        let checkout = &job.steps[1];
        match &checkout.reference {
            StepReference::Repository { name, r#ref, .. } => {
                assert_eq!(name, "actions/checkout");
                assert_eq!(r#ref, "v4");
            }
            other => panic!("expected repository reference, got {other:?}"),
        }
    }

    #[test]
    fn envelope_classifies_messages() {
        let job = Envelope {
            message_id: Some(7),
            message_type: "RunnerJobRequest".into(),
            body: r#"{"runServiceUrl":"https://run.example","runnerRequestId":"req-1","billingOwnerId":"o","shouldAcknowledge":true}"#.into(),
            iv: None,
        };
        let offer = job.job_offer().unwrap();
        assert_eq!(offer.runner_request_id, "req-1");
        assert!(offer.should_acknowledge);
        assert!(!job.is_inline_job());

        let cancel = Envelope {
            message_id: Some(8),
            message_type: "JobCancelRequest".into(),
            body: "{}".into(),
            iv: None,
        };
        assert!(cancel.is_cancel());

        let migrate = Envelope {
            message_id: None,
            message_type: "BrokerMigration".into(),
            body: r#"{"brokerBaseUrl":"https://broker.example"}"#.into(),
            iv: None,
        };
        assert_eq!(
            migrate.broker_url().as_deref(),
            Some("https://broker.example")
        );

        let update = Envelope {
            message_id: Some(9),
            message_type: "RunnerRefreshMessage".into(),
            body: "{}".into(),
            iv: None,
        };
        assert!(update.is_update());
    }
}
