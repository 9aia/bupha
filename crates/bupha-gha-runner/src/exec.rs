//! Job execution: expanded [`JobMessage`] steps → spawned shell
//! children in a per-job workspace. Script steps run through bash/sh,
//! `actions/checkout` clones the repository, and the `$GITHUB_*`
//! file commands (`GITHUB_ENV`/`GITHUB_OUTPUT`/`GITHUB_PATH`) the
//! protocol requires are implemented per job.
//!
//! Everything unsupported fails the *step* loudly — a node action we
//! cannot run is a red step naming the action, never a silent skip.

use serde_json::Value;
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::types::*;

/// Live progress hook — the agent's reporter implements it to push
/// step states and stream log lines; tests use [`NullProgress`].
pub trait Progress: Send {
    /// A step began running (index into `job.steps`).
    fn step_started(&mut self, _pos: usize, _step: &PipelineStep) {}
    /// One output line from the running step.
    fn line(&mut self, _line: &str) {}
    /// A step reached a conclusion.
    fn step_finished(&mut self, _pos: usize, _result: &StepResult) {}
    /// Buffered output should be flushed (step boundary).
    fn flush(&mut self) {}
}

/// Discards everything.
pub struct NullProgress;

impl Progress for NullProgress {}

/// Cancellation + the in-flight child, shared between the agent's
/// message loop and the executor. The child sits behind a mutex so
/// `cancel()` can kill it while the wait loop polls `try_wait`.
#[derive(Default)]
pub struct JobRunner {
    cancel: Arc<AtomicBool>,
    child: Mutex<Option<Arc<Mutex<Child>>>>,
}

impl JobRunner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ask the running job to stop — kills the current step's child;
    /// the wait loop sees the flag and reports `canceled`.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        if let Some(shared) = self.child.lock().unwrap().as_ref() {
            let _ = shared.lock().unwrap().kill();
        }
    }

    pub fn cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    fn set_child(&self, child: Option<Arc<Mutex<Child>>>) {
        *self.child.lock().unwrap() = child;
    }
}

/// What one job run produced.
pub struct JobOutcome {
    pub conclusion: Outcome,
    /// Per-step results in wire shape, ready for `completejob`.
    pub steps: Vec<StepResult>,
    /// step backend id → captured output, for log upload.
    pub step_logs: BTreeMap<String, String>,
    /// step id → the `$GITHUB_OUTPUT` name/value pairs that step
    /// wrote — the raw material `steps.<id>.outputs.*` reads.
    pub outputs: BTreeMap<String, BTreeMap<String, String>>,
    /// The job's `outputs:` map evaluated against `outputs` — what
    /// `completejob` reports so `needs.<job>.outputs.*` resolves on
    /// dependent jobs.
    pub job_outputs: BTreeMap<String, String>,
    /// The concatenated job log.
    pub job_log: String,
}

/// Per-job scratch: env files, the event payload, script files.
/// Lives next to the workspace under `<workdir>/.bupha-gha-runner/`.
struct Scratch {
    dir: PathBuf,
    env_file: PathBuf,
    output_file: PathBuf,
    path_file: PathBuf,
    event_file: PathBuf,
    summary_file: PathBuf,
}

/// The job workspace's parent — where `.bupha-gha-runner/` scratch
/// lives. `ws` is `<parent>/<job_id>`.
fn scratch_root(ws: &Path) -> PathBuf {
    ws.parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| ws.to_path_buf())
}

fn scratch_dir_name(job: &JobMessage) -> String {
    job.job_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Scratch {
    fn new(ws: &Path, job: &JobMessage) -> std::io::Result<Self> {
        let dir = scratch_root(ws).join(format!(".bupha-gha-runner/{}", scratch_dir_name(job)));
        std::fs::create_dir_all(&dir)?;
        let tmp = dir.join("tmp");
        std::fs::create_dir_all(&tmp)?;
        Ok(Self {
            env_file: dir.join("github_env"),
            output_file: dir.join("github_output"),
            path_file: dir.join("github_path"),
            event_file: dir.join("github_event.json"),
            summary_file: dir.join("step_summary.md"),
            dir: tmp,
        })
    }
}

/// The job's mutable environment: base vars, env-file additions and
/// PATH prepends accumulate across steps.
struct JobEnv {
    vars: BTreeMap<String, String>,
    /// Extra PATH entries, most-recent first.
    path_dirs: Vec<PathBuf>,
    /// `GITHUB_OUTPUT` values per step — collected, keyed by step id.
    outputs: BTreeMap<String, BTreeMap<String, String>>,
}

impl JobEnv {
    fn set(&mut self, key: &str, value: &str) {
        self.vars.insert(key.to_string(), value.to_string());
    }

    /// The environment for one step's child: accumulated vars with
    /// PATH rebuilt from `path_dirs` (GITHUB_PATH prepends).
    fn for_step(&self) -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = self
            .vars
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if !self.path_dirs.is_empty() {
            let mut path = String::new();
            for d in &self.path_dirs {
                if !path.is_empty() {
                    path.push(':');
                }
                path.push_str(&d.display().to_string());
            }
            if let Some(existing) = self.vars.get("PATH") {
                path.push(':');
                path.push_str(existing);
            }
            out.retain(|(k, _)| k != "PATH");
            out.push(("PATH".to_string(), path));
        }
        out
    }
}

/// Run every step of `job` in `ws`. `runner` carries cancellation;
/// `progress` receives live step/log events.
pub fn execute(
    job: &JobMessage,
    ws: &Path,
    runner: &JobRunner,
    progress: &mut dyn Progress,
) -> JobOutcome {
    let mut results = Vec::with_capacity(job.steps.len());
    let mut step_logs: BTreeMap<String, String> = BTreeMap::new();
    let mut job_log = String::new();
    let mut status = JobStatus::Success;

    let scratch = match Scratch::new(ws, job) {
        Ok(s) => s,
        Err(e) => {
            return JobOutcome {
                conclusion: Outcome::Failed,
                steps: results,
                step_logs,
                outputs: BTreeMap::new(),
                job_outputs: BTreeMap::new(),
                job_log: format!("gha-runner: cannot create scratch dir: {e}\n"),
            };
        }
    };
    let mut env = prepare_env(job, ws, &scratch);
    wire_scratch(&mut env, &scratch, job);

    for (i, step) in job.steps.iter().enumerate() {
        if runner.cancelled() {
            status = JobStatus::Cancelled;
            break;
        }
        let started = Instant::now();
        let cond = match eval_condition(step.condition.as_deref(), status, runner.cancelled()) {
            Ok(c) => c,
            Err(e) => {
                let msg = format!("::error::{e}\n");
                job_log.push_str(&msg);
                let result = finish_result(step, i, "failed", started);
                progress.step_finished(i, &result);
                results.push(result);
                status = JobStatus::Failure;
                continue;
            }
        };
        if !cond {
            let result = finish_result(step, i, "skipped", started);
            results.push(result.clone());
            progress.step_finished(i, &result);
            continue;
        }

        progress.step_started(i, step);
        let outcome = run_step(step, i, job, ws, &scratch, &mut env, runner, progress);
        step_logs.insert(step.id.clone(), outcome.log.clone());
        job_log.push_str(&outcome.log);
        let result = finish_result(step, i, outcome.conclusion.step_name(), started);
        progress.step_finished(i, &result);
        results.push(result);

        match outcome.conclusion {
            StepConclusion::Succeeded => {}
            StepConclusion::Failed if step.continue_on_error => {
                job_log.push_str(&format!(
                    "::warning::step '{}' failed but continue-on-error is set\n",
                    step.label()
                ));
            }
            StepConclusion::Failed => status = JobStatus::Failure,
            StepConclusion::Cancelled => {
                status = JobStatus::Cancelled;
                break;
            }
        }
    }

    // Steps never reached still report — the service expects the
    // full step list.
    for (i, step) in job.steps.iter().enumerate().skip(results.len()) {
        results.push(finish_result(step, i, "skipped", Instant::now()));
    }

    let conclusion = match status {
        JobStatus::Success => Outcome::Succeeded,
        JobStatus::Failure => Outcome::Failed,
        JobStatus::Cancelled => Outcome::Cancelled,
    };
    let job_outputs = resolve_job_outputs(job, &env.outputs, &env.vars);
    JobOutcome {
        conclusion,
        steps: results,
        step_logs,
        outputs: env.outputs,
        job_outputs,
        job_log,
    }
}

// ------------------------------------------------------------------
// job outputs — the `outputs:` map evaluated against step outputs
// ------------------------------------------------------------------

/// Evaluate the job's `outputs:` templates (`job.job_outputs`) into
/// the name→value map `completejob` reports. Supported refs:
/// `steps.<id>.outputs.<name>` (collected `$GITHUB_OUTPUT` values),
/// `needs.<job>.outputs.<name>` (the `needs` context the service
/// pre-fills), `env.<NAME>` and `github.<key>` scalars. An output
/// whose template uses anything else is left unset rather than
/// reported wrong — the full `${{ }}` engine is the service's.
fn resolve_job_outputs(
    job: &JobMessage,
    step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
    env: &BTreeMap<String, String>,
) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for (name, tmpl) in &job.job_outputs {
        if let Some(v) = eval_output_template(tmpl, job, step_outputs, env) {
            out.insert(name.clone(), v);
        }
    }
    out
}

/// `${{ … }}` substitution over an outputs template. `None` when an
/// expression falls outside the supported refs.
fn eval_output_template(
    tmpl: &str,
    job: &JobMessage,
    step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    let mut out = String::with_capacity(tmpl.len());
    let mut rest = tmpl;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let end = after.find("}}")?;
        out.push_str(&eval_output_expr(
            after[..end].trim(),
            job,
            step_outputs,
            env,
        )?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Some(out)
}

fn eval_output_expr(
    inner: &str,
    job: &JobMessage,
    step_outputs: &BTreeMap<String, BTreeMap<String, String>>,
    env: &BTreeMap<String, String>,
) -> Option<String> {
    // Same first-`.outputs.` split the dialect renderer uses.
    if let Some(rest) = inner.strip_prefix("steps.") {
        let (sid, name) = rest.split_once(".outputs.")?;
        if sid.is_empty() || name.is_empty() {
            return None;
        }
        return Some(
            step_outputs
                .get(sid)
                .and_then(|m| m.get(name))
                .cloned()
                .unwrap_or_default(),
        );
    }
    if let Some(rest) = inner.strip_prefix("needs.") {
        let (job_id, name) = rest.split_once(".outputs.")?;
        if job_id.is_empty() || name.is_empty() {
            return None;
        }
        return Some(needs_output(job, job_id, name));
    }
    if let Some(name) = inner.strip_prefix("env.") {
        if name.is_empty() {
            return None;
        }
        return Some(env.get(name).cloned().unwrap_or_default());
    }
    if let Some(key) = inner.strip_prefix("github.") {
        if key.is_empty() {
            return None;
        }
        return Some(scalar_at(&job.context["github"], key));
    }
    None
}

/// `context.needs.<job>.outputs.<name>` — the service fills `needs`
/// with each dependency's reported outputs (and `result`).
fn needs_output(job: &JobMessage, job_id: &str, name: &str) -> String {
    job.context
        .get("needs")
        .and_then(|n| n.get(job_id))
        .and_then(|j| j.get("outputs"))
        .and_then(|o| o.get(name))
        .map(scalar_json)
        .unwrap_or_default()
}

fn scalar_at(v: &Value, key: &str) -> String {
    v.get(key).map(scalar_json).unwrap_or_default()
}

fn scalar_json(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        Value::Bool(b) => b.to_string(),
        _ => String::new(),
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum JobStatus {
    Success,
    Failure,
    Cancelled,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StepConclusion {
    Succeeded,
    Failed,
    Cancelled,
}

impl StepConclusion {
    fn step_name(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

fn finish_result(
    step: &PipelineStep,
    i: usize,
    conclusion: &'static str,
    started: Instant,
) -> StepResult {
    let name = if step.display_name.is_empty() {
        step.name.clone()
    } else {
        step.display_name.clone()
    };
    StepResult {
        external_id: step.id.clone(),
        number: (i + 1) as u32,
        name,
        status: "completed",
        conclusion: Some(conclusion),
        started_at: Some(timestamp_of(started)),
        completed_at: Some(timestamp(std::time::SystemTime::now())),
    }
}

fn timestamp_of(started: Instant) -> String {
    // Instant has no wall clock; approximate via now minus elapsed.
    let now = std::time::SystemTime::now();
    timestamp(now - started.elapsed())
}

// ------------------------------------------------------------------
// environment
// ------------------------------------------------------------------

/// Build the job's base environment from the github context, runner
/// facts and the service's environment variables. Step env and
/// env-file additions layer on top per step.
fn prepare_env(job: &JobMessage, ws: &Path, scratch: &Scratch) -> JobEnv {
    let mut vars = BTreeMap::new();
    vars.insert("CI".to_string(), "true".to_string());
    vars.insert("GITHUB_ACTIONS".to_string(), "true".to_string());
    vars.insert("GITHUB_WORKSPACE".to_string(), ws.display().to_string());

    // github.* context → GITHUB_* env, scalar values only.
    if let Some(github) = job.context.get("github").and_then(Value::as_object) {
        for (k, v) in github {
            let name = format!("GITHUB_{}", k.to_uppercase().replace('-', "_"));
            match v {
                Value::String(s) => {
                    vars.insert(name, s.clone());
                }
                Value::Number(n) => {
                    vars.insert(name, n.to_string());
                }
                Value::Bool(b) => {
                    vars.insert(name, b.to_string());
                }
                _ => {}
            }
        }
    }
    vars.insert("GITHUB_JOB".to_string(), job.job_name.clone());

    // Runner facts.
    vars.insert("RUNNER_OS".to_string(), crate::host_os().to_string());
    vars.insert("RUNNER_ARCH".to_string(), crate::host_arch().to_string());
    vars.insert(
        "RUNNER_NAME".to_string(),
        job.variable("system.runner.name")
            .or_else(|| job.variable("runner.name"))
            .unwrap_or("bupha")
            .to_string(),
    );
    vars.insert("RUNNER_ENVIRONMENT".to_string(), "self-hosted".to_string());
    vars.insert("RUNNER_TEMP".to_string(), scratch.dir.display().to_string());
    vars.insert(
        "RUNNER_TOOL_CACHE".to_string(),
        scratch_root(ws)
            .join(".bupha-gha-runner/toolcache")
            .display()
            .to_string(),
    );
    // Inherited PATH so scripts find system tools; HOME for tools
    // that want a home.
    if let Ok(path) = std::env::var("PATH") {
        vars.insert("PATH".to_string(), path);
    }
    if let Ok(home) = std::env::var("HOME") {
        vars.insert("HOME".to_string(), home);
    }
    // Service-provided environment (secrets land here named).
    for (k, v) in &job.environment_variables {
        vars.insert(k.clone(), v.clone());
    }
    JobEnv {
        vars,
        path_dirs: Vec::new(),
        outputs: BTreeMap::new(),
    }
}

/// Point the `$GITHUB_*` file variables at the scratch files and
/// make sure they exist so `>>` never fails on a missing path.
fn wire_scratch(env: &mut JobEnv, scratch: &Scratch, job: &JobMessage) {
    env.set("GITHUB_ENV", &scratch_file(scratch, "env_file"));
    env.set("GITHUB_OUTPUT", &scratch_file(scratch, "output_file"));
    env.set("GITHUB_PATH", &scratch_file(scratch, "path_file"));
    env.set("GITHUB_EVENT_PATH", &scratch_file(scratch, "event_file"));
    env.set(
        "GITHUB_STEP_SUMMARY",
        &scratch_file(scratch, "summary_file"),
    );
    for f in [
        scratch.env_file(),
        scratch.output_file(),
        scratch.path_file(),
        scratch.event_file(),
        scratch.summary_file(),
    ] {
        let _ = std::fs::write(f, "");
    }
    // The event payload the context carried.
    if let Some(event) = job.context.get("github").and_then(|g| g.get("event")) {
        let _ = std::fs::write(
            scratch.event_file(),
            serde_json::to_string_pretty(event).unwrap_or_default(),
        );
    }
}

impl Scratch {
    fn env_file(&self) -> &Path {
        &self.env_file
    }
    fn output_file(&self) -> &Path {
        &self.output_file
    }
    fn path_file(&self) -> &Path {
        &self.path_file
    }
    fn event_file(&self) -> &Path {
        &self.event_file
    }
    fn summary_file(&self) -> &Path {
        &self.summary_file
    }
}

fn scratch_file(scratch: &Scratch, which: &str) -> String {
    let p = match which {
        "env_file" => &scratch.env_file,
        "output_file" => &scratch.output_file,
        "path_file" => &scratch.path_file,
        "event_file" => &scratch.event_file,
        _ => &scratch.summary_file,
    };
    p.display().to_string()
}

// ------------------------------------------------------------------
// conditions — one pass: parse and evaluate against live status
// ------------------------------------------------------------------

/// Evaluate a step condition. The service emits expressions like
/// `success()`; the default (no `if:`) is `success()`. Supported:
/// `success()`, `failure()`, `cancelled()`, `always()`, `!`, `&&`,
/// `||`, parentheses, `true`/`false`. Anything else errors loudly.
fn eval_condition(expr: Option<&str>, status: JobStatus, cancelled: bool) -> Result<bool, String> {
    let expr = expr.map(str::trim).unwrap_or("");
    if expr.is_empty() {
        return Ok(status == JobStatus::Success && !cancelled);
    }
    let tokens = lex(expr)?;
    let mut p = CondParser {
        tokens,
        pos: 0,
        status,
        cancelled,
    };
    let v = p.parse_or()?;
    if p.pos != p.tokens.len() {
        return Err(format!("condition `{expr}`: trailing input"));
    }
    Ok(v)
}

#[derive(Clone, Debug, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Not,
    And,
    Or,
    Word(String),
}

fn lex(expr: &str) -> Result<Vec<Tok>, String> {
    let chars: Vec<char> = expr.chars().collect();
    let mut out = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        match chars[i] {
            c if c.is_whitespace() => i += 1,
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '!' => {
                out.push(Tok::Not);
                i += 1;
            }
            '&' if chars.get(i + 1) == Some(&'&') => {
                out.push(Tok::And);
                i += 2;
            }
            '|' if chars.get(i + 1) == Some(&'|') => {
                out.push(Tok::Or);
                i += 2;
            }
            c if c.is_ascii_alphanumeric() || c == '_' || c == '.' || c == '\'' => {
                let start = i;
                while i < chars.len() {
                    let c = chars[i];
                    let two: String = chars[i..].iter().take(2).collect();
                    if two == "&&" || two == "||" {
                        break;
                    }
                    if c.is_whitespace() {
                        break;
                    }
                    if !(c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '\'' | '-')) {
                        break;
                    }
                    i += 1;
                }
                let mut word: String = chars[start..i].iter().collect();
                // `success()` lexes as a word followed by parens —
                // fold the empty pair in so atoms match whole.
                if chars.get(i) == Some(&'(') && chars.get(i + 1) == Some(&')') {
                    word.push_str("()");
                    i += 2;
                }
                if !word.is_empty() {
                    out.push(Tok::Word(word));
                }
            }
            other => return Err(format!("condition: unexpected `{other}`")),
        }
    }
    Ok(out)
}

struct CondParser {
    tokens: Vec<Tok>,
    pos: usize,
    status: JobStatus,
    cancelled: bool,
}

impl CondParser {
    fn peek(&self) -> Option<&Tok> {
        self.tokens.get(self.pos)
    }
    fn bump(&mut self) -> Option<Tok> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }
    fn parse_or(&mut self) -> Result<bool, String> {
        let mut v = self.parse_and()?;
        while self.peek() == Some(&Tok::Or) {
            self.bump();
            let r = self.parse_and()?;
            v = v || r;
        }
        Ok(v)
    }
    fn parse_and(&mut self) -> Result<bool, String> {
        let mut v = self.parse_unary()?;
        while self.peek() == Some(&Tok::And) {
            self.bump();
            let r = self.parse_unary()?;
            v = v && r;
        }
        Ok(v)
    }
    fn parse_unary(&mut self) -> Result<bool, String> {
        if self.peek() == Some(&Tok::Not) {
            self.bump();
            return Ok(!self.parse_unary()?);
        }
        self.parse_primary()
    }
    fn parse_primary(&mut self) -> Result<bool, String> {
        match self.bump() {
            Some(Tok::LParen) => {
                let v = self.parse_or()?;
                match self.bump() {
                    Some(Tok::RParen) => Ok(v),
                    _ => Err("condition: unclosed `(`".into()),
                }
            }
            Some(Tok::Word(w)) => self.atom(&w),
            other => Err(format!("condition: unexpected {other:?}")),
        }
    }
    fn atom(&self, word: &str) -> Result<bool, String> {
        match word {
            "success()" => Ok(self.status == JobStatus::Success && !self.cancelled),
            "failure()" => Ok(self.status == JobStatus::Failure),
            "cancelled()" => Ok(self.cancelled || self.status == JobStatus::Cancelled),
            "always()" => Ok(true),
            "true" | "true()" => Ok(true),
            "false" | "false()" => Ok(false),
            other => Err(format!(
                "unsupported condition atom `{other}` — supported: success(), failure(), cancelled(), always(), !, &&, ||"
            )),
        }
    }
}

// ------------------------------------------------------------------
// step execution
// ------------------------------------------------------------------

struct StepOutput {
    conclusion: StepConclusion,
    log: String,
}

#[allow(clippy::too_many_arguments)]
fn run_step(
    step: &PipelineStep,
    i: usize,
    job: &JobMessage,
    ws: &Path,
    scratch: &Scratch,
    env: &mut JobEnv,
    runner: &JobRunner,
    progress: &mut dyn Progress,
) -> StepOutput {
    match &step.reference {
        StepReference::Script => run_script(step, i, ws, scratch, env, runner, progress),
        StepReference::Repository { name, r#ref, .. } => {
            if name.eq_ignore_ascii_case("actions/checkout") {
                run_checkout(step, job, ws, env, runner)
            } else {
                StepOutput {
                    conclusion: StepConclusion::Failed,
                    log: format!(
                        "::error::unsupported action `{name}@{}` — bupha-gha-runner executes script steps and actions/checkout only\n",
                        if r#ref.is_empty() { "?" } else { r#ref }
                    ),
                }
            }
        }
        StepReference::ContainerRegistry { image } => StepOutput {
            conclusion: StepConclusion::Failed,
            log: format!("::error::container step `{image}` is unsupported\n"),
        },
        StepReference::Other { raw } => StepOutput {
            conclusion: StepConclusion::Failed,
            log: format!("::error::unsupported step reference {raw}\n"),
        },
    }
}

fn run_script(
    step: &PipelineStep,
    i: usize,
    ws: &Path,
    scratch: &Scratch,
    env: &mut JobEnv,
    runner: &JobRunner,
    progress: &mut dyn Progress,
) -> StepOutput {
    let script = step.inputs.get("script").cloned().unwrap_or_default();
    if script.trim().is_empty() {
        return StepOutput {
            conclusion: StepConclusion::Failed,
            log: "::error::run step has an empty script\n".into(),
        };
    }
    let script_file = scratch.dir.join(format!("step-{i}.sh"));
    if let Err(e) = std::fs::write(&script_file, &script) {
        return StepOutput {
            conclusion: StepConclusion::Failed,
            log: format!("::error::cannot write script file: {e}\n"),
        };
    }
    let shell = step
        .inputs
        .get("shell")
        .cloned()
        .unwrap_or_else(|| "bash".to_string());
    let mut cmd = match shell.as_str() {
        "bash" => {
            let mut c = Command::new("bash");
            c.args(["--noprofile", "--norc", "-eo", "pipefail", "--"])
                .arg(&script_file);
            c
        }
        "sh" => {
            let mut c = Command::new("sh");
            c.arg("-e").arg(&script_file);
            c
        }
        other => {
            return StepOutput {
                conclusion: StepConclusion::Failed,
                log: format!("::error::unsupported shell `{other}`\n"),
            }
        }
    };
    let wd = step
        .inputs
        .get("working-directory")
        .map(|w| ws.join(w))
        .unwrap_or_else(|| ws.to_path_buf());
    if wd.is_absolute() && !wd.starts_with(ws) {
        return StepOutput {
            conclusion: StepConclusion::Failed,
            log: format!(
                "::error::working-directory escapes the workspace: {}\n",
                wd.display()
            ),
        };
    }
    cmd.current_dir(&wd);
    apply_step_env(&mut cmd, step, env);
    let timeout = step
        .timeout_in_minutes
        .map(|m| Duration::from_secs(m.saturating_mul(60)));
    match wait_child(cmd, timeout, runner, progress) {
        Ok((true, log, _)) => {
            absorb_env_files(scratch, env, step.id.as_str());
            StepOutput {
                conclusion: StepConclusion::Succeeded,
                log,
            }
        }
        Ok((false, log, err)) => {
            absorb_env_files(scratch, env, step.id.as_str());
            let (conclusion, err) = if runner.cancelled() {
                (StepConclusion::Cancelled, "cancelled".to_string())
            } else {
                (
                    StepConclusion::Failed,
                    err.unwrap_or_else(|| "exit status non-zero".to_string()),
                )
            };
            StepOutput {
                conclusion,
                log: format!("{log}\n::error::{err}\n"),
            }
        }
        Err(e) => StepOutput {
            conclusion: StepConclusion::Failed,
            log: format!("::error::spawn failed: {e}\n"),
        },
    }
}

fn apply_step_env(cmd: &mut Command, step: &PipelineStep, env: &JobEnv) {
    cmd.env_clear();
    for (k, v) in env.for_step() {
        cmd.env(&k, &v);
    }
    for (k, v) in &step.env {
        cmd.env(k, v);
    }
}

/// Spawn + wait with output capture, timeout and cancellation. The
/// child is shared with `JobRunner` so `cancel()` can kill it mid-
/// wait; the wait loop also re-checks the flag every 50ms.
fn wait_child(
    mut cmd: Command,
    timeout: Option<Duration>,
    runner: &JobRunner,
    progress: &mut dyn Progress,
) -> std::io::Result<(bool, String, Option<String>)> {
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = cmd.spawn()?;
    let shared = Arc::new(Mutex::new(child));
    runner.set_child(Some(shared.clone()));

    let mut out = shared.lock().unwrap().stdout.take().expect("stdout piped");
    let mut err = shared.lock().unwrap().stderr.take().expect("stderr piped");
    let (tx, rx) = std::sync::mpsc::channel::<Vec<u8>>();
    let tx2 = tx.clone();
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = out.read_to_end(&mut buf);
        let _ = tx.send(buf);
    });
    std::thread::spawn(move || {
        let mut buf = Vec::new();
        let _ = err.read_to_end(&mut buf);
        let _ = tx2.send(buf);
    });

    let deadline = timeout.map(|t| Instant::now() + t);
    let status = loop {
        if runner.cancelled() {
            let _ = shared.lock().unwrap().kill();
            let _ = shared.lock().unwrap().wait();
            runner.set_child(None);
            let log = drain(rx, progress);
            return Ok((false, log, Some("cancelled".into())));
        }
        let s = shared.lock().unwrap().try_wait()?;
        match s {
            Some(s) => break s,
            None => {
                if let Some(d) = deadline {
                    if Instant::now() >= d {
                        let _ = shared.lock().unwrap().kill();
                        let _ = shared.lock().unwrap().wait();
                        runner.set_child(None);
                        let log = drain(rx, progress);
                        return Ok((false, log, Some("timed out".into())));
                    }
                }
                std::thread::sleep(Duration::from_millis(50));
            }
        }
    };
    runner.set_child(None);
    let log = drain(rx, progress);
    Ok((status.success(), log, None))
}

fn drain(rx: std::sync::mpsc::Receiver<Vec<u8>>, progress: &mut dyn Progress) -> String {
    let mut log = String::new();
    // Two chunks: stdout then stderr (order between streams is not
    // preserved — the official runner interleaves through one pipe).
    for chunk in rx.iter().take(2) {
        log.push_str(&String::from_utf8_lossy(&chunk));
    }
    for line in log.lines() {
        progress.line(&format!("{line}\n"));
    }
    progress.flush();
    log
}

// ------------------------------------------------------------------
// actions/checkout
// ------------------------------------------------------------------

fn run_checkout(
    step: &PipelineStep,
    job: &JobMessage,
    ws: &Path,
    env: &JobEnv,
    runner: &JobRunner,
) -> StepOutput {
    if let StepReference::Repository {
        repository_type, ..
    } = &step.reference
    {
        if !repository_type.is_empty() && !repository_type.eq_ignore_ascii_case("github") {
            return StepOutput {
                conclusion: StepConclusion::Failed,
                log: format!("::error::actions/checkout from `{repository_type}` is unsupported\n"),
            };
        }
    }
    // The job isn't threaded through run_step's signature for env
    // reasons — recover the github context fields we need from the
    // step env (GITHUB_* were exported there) plus workspace .git.
    let repo = env
        .vars
        .get("GITHUB_REPOSITORYURL")
        .cloned()
        .unwrap_or_default();
    let repository = env
        .vars
        .get("GITHUB_REPOSITORY")
        .cloned()
        .unwrap_or_default();
    let server = env
        .vars
        .get("GITHUB_SERVER_URL")
        .cloned()
        .unwrap_or_else(|| "https://github.com".to_string());
    let sha = env.vars.get("GITHUB_SHA").cloned().unwrap_or_default();
    let git_ref = env.vars.get("GITHUB_REF").cloned().unwrap_or_default();
    let explicit_ref = step.inputs.get("ref").cloned();
    let checkout_ref = explicit_ref.unwrap_or_else(|| {
        if sha.is_empty() {
            git_ref.clone()
        } else {
            sha.clone()
        }
    });
    let url = checkout_url(&repo, &repository, &server);
    // The job's own token (system.github.token) — env only carries
    // it when the service chose to export it.
    let token = job
        .variable("system.github.token")
        .or_else(|| job.variable("github_token"))
        .or_else(|| env.vars.get("GITHUB_TOKEN").map(String::as_str))
        .unwrap_or_default()
        .to_string();
    match checkout_repo(&url, &checkout_ref, &token, ws, runner) {
        Ok(log) => StepOutput {
            conclusion: StepConclusion::Succeeded,
            log,
        },
        Err(e) => StepOutput {
            conclusion: StepConclusion::Failed,
            log: format!("::error::actions/checkout failed: {e}\n"),
        },
    }
}

/// `git://github.com/o/r.git` → `https://…`; bare `o/r` gets the
/// server prefix.
fn checkout_url(repository_url: &str, repository: &str, server: &str) -> String {
    if !repository_url.is_empty() {
        if let Some(rest) = repository_url.strip_prefix("git://") {
            return format!("https://{rest}");
        }
        if repository_url.starts_with("http://") || repository_url.starts_with("https://") {
            return repository_url.to_string();
        }
    }
    if repository.is_empty() {
        return String::new();
    }
    format!("{server}/{repository}.git")
}

/// The `Authorization: Basic` value for `x-access-token:TOKEN`, or
/// None for anonymous clones.
fn git_auth(token: &str) -> Option<String> {
    if token.is_empty() {
        return None;
    }
    use base64::Engine as _;
    Some(base64::engine::general_purpose::STANDARD.encode(format!("x-access-token:{token}")))
}

fn git(
    dir: &Path,
    args: &[&str],
    auth: Option<&str>,
    runner: &JobRunner,
    log: &mut String,
) -> Result<(), String> {
    let mut c = Command::new("git");
    c.arg("-C").arg(dir);
    if let Some(b64) = auth {
        c.args([
            "-c",
            &format!("http.extraheader=AUTHORIZATION: basic {b64}"),
        ]);
    }
    c.args(args);
    let out = c.output().map_err(|e| format!("git {:?}: {e}", args))?;
    if runner.cancelled() {
        return Err("cancelled".into());
    }
    log.push_str(&String::from_utf8_lossy(&out.stdout));
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    if out.status.success() {
        Ok(())
    } else {
        Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&out.stderr).trim()
        ))
    }
}

/// Clone (or reuse) the repository at `checkout_ref`.
fn checkout_repo(
    url: &str,
    checkout_ref: &str,
    token: &str,
    ws: &Path,
    runner: &JobRunner,
) -> Result<String, String> {
    if url.is_empty() {
        return Err("no repository URL in the job context".into());
    }
    if runner.cancelled() {
        return Err("cancelled".into());
    }
    let auth = git_auth(token);
    let auth_ref = auth.as_deref();
    let mut log = String::new();
    if ws.join(".git").exists() {
        // Already cloned (an earlier checkout step) — fetch + checkout.
        git(
            ws,
            &["fetch", "origin", checkout_ref],
            auth_ref,
            runner,
            &mut log,
        )
        .or_else(|_| git(ws, &["fetch", "origin"], auth_ref, runner, &mut log))?;
        git(ws, &["checkout", checkout_ref], auth_ref, runner, &mut log)?;
        return Ok(log);
    }
    if ws
        .read_dir()
        .map(|mut d| d.next().is_some())
        .unwrap_or(false)
    {
        return Err(format!(
            "workspace {} is not empty and has no .git — refusing to clobber it",
            ws.display()
        ));
    }
    // Clone with --no-checkout; the token rides http.extraheader so
    // it never lands in .git/config.
    let mut c = Command::new("git");
    c.arg("-C").arg(ws.parent().unwrap_or(ws));
    if let Some(b64) = auth_ref {
        c.args([
            "-c",
            &format!("http.extraheader=AUTHORIZATION: basic {b64}"),
        ]);
    }
    c.args(["clone", "--no-checkout", url])
        .arg(ws.file_name().unwrap_or_default());
    let out = c.output().map_err(|e| format!("git clone: {e}"))?;
    log.push_str(&String::from_utf8_lossy(&out.stdout));
    log.push_str(&String::from_utf8_lossy(&out.stderr));
    if !out.status.success() || runner.cancelled() {
        return Err(format!(
            "git clone failed: {}",
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    if !checkout_ref.is_empty()
        && git(ws, &["checkout", checkout_ref], auth_ref, runner, &mut log).is_err()
    {
        git(
            ws,
            &["fetch", "origin", checkout_ref],
            auth_ref,
            runner,
            &mut log,
        )
        .or_else(|_| git(ws, &["fetch", "origin"], auth_ref, runner, &mut log))?;
        git(ws, &["checkout", "FETCH_HEAD"], auth_ref, runner, &mut log)?;
    }
    Ok(log)
}

// ------------------------------------------------------------------
// $GITHUB_ENV / $GITHUB_OUTPUT / $GITHUB_PATH
// ------------------------------------------------------------------

/// Fold the env files a step wrote back into the job environment.
/// Supports `KEY=VALUE` lines and `KEY<<DELIM` heredocs — the two
/// forms the runner contract documents.
fn absorb_env_files(scratch: &Scratch, env: &mut JobEnv, step_id: &str) {
    absorb_file(scratch.env_file(), false, |k, v| env.set(&k, &v));
    let mut outputs = BTreeMap::new();
    absorb_file(scratch.output_file(), true, |k, v| {
        outputs.insert(k, v);
    });
    if !outputs.is_empty() {
        env.outputs.insert(step_id.to_string(), outputs);
    }
    // GITHUB_PATH is line-per-directory; each line prepends to PATH,
    // so within a file the LAST line wins and a later step's entries
    // outrank earlier steps'. `path_dirs` is most-recent-first — the
    // reversed block goes in front, not at the back.
    if let Ok(text) = std::fs::read_to_string(scratch.path_file()) {
        let mut dirs: Vec<PathBuf> = text
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .map(PathBuf::from)
            .collect();
        dirs.reverse();
        dirs.extend(std::mem::take(&mut env.path_dirs));
        env.path_dirs = dirs;
    }
    // Each step starts with clean command files — what a step wrote
    // has been folded in; appends *within* a step are preserved
    // because we only truncate after absorbing.
    for f in [
        scratch.env_file(),
        scratch.output_file(),
        scratch.path_file(),
    ] {
        let _ = std::fs::write(f, "");
    }
}

/// Parse a GitHub env file: `NAME=VALUE`, `NAME<<DELIM` heredocs,
/// comments. Calls `sink(name, value)` per entry. `for_output` relaxes
/// the name rule for `$GITHUB_OUTPUT`, where hyphens are legal
/// (`steps.x.outputs.my-output` is a real workflow pattern); env
/// names stay strict shell-identifier-shaped.
fn absorb_file(path: &Path, for_output: bool, mut sink: impl FnMut(String, String)) {
    let Ok(text) = std::fs::read_to_string(path) else {
        return;
    };
    let mut lines = text.lines().peekable();
    while let Some(line) = lines.next() {
        let line = line.trim_end_matches('\r');
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some((name, rest)) = line.split_once("<<") {
            let name = name.trim();
            if valid_file_name(name, for_output) {
                let delim = rest
                    .trim()
                    .trim_matches(|c| c == '\'' || c == '"')
                    .to_string();
                if !delim.is_empty() {
                    let mut body = String::new();
                    for l in lines.by_ref() {
                        if l.trim_end_matches('\r') == delim {
                            break;
                        }
                        body.push_str(l);
                        body.push('\n');
                    }
                    sink(name.to_string(), body);
                    continue;
                }
            }
        }
        if let Some((name, value)) = line.split_once('=') {
            let name = name.trim();
            if valid_file_name(name, for_output) {
                sink(name.to_string(), value.to_string());
            }
        }
    }
}

/// Env-file entry names: `[A-Za-z_][A-Za-z0-9_]*`, plus `-` when the
/// file is `$GITHUB_OUTPUT` (output ids may be hyphenated).
fn valid_file_name(name: &str, for_output: bool) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|c| c.is_ascii_alphanumeric() || c == '_' || (for_output && c == '-'))
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn cond(expr: Option<&str>, status: JobStatus, cancelled: bool) -> bool {
        eval_condition(expr, status, cancelled).unwrap()
    }

    #[test]
    fn conditions_evaluate_against_job_status() {
        assert!(cond(None, JobStatus::Success, false));
        assert!(!cond(None, JobStatus::Failure, false));
        assert!(cond(Some("always()"), JobStatus::Failure, false));
        assert!(cond(Some("failure()"), JobStatus::Failure, false));
        assert!(!cond(Some("failure()"), JobStatus::Success, false));
        assert!(cond(Some("cancelled()"), JobStatus::Success, true));
        assert!(cond(Some("!success()"), JobStatus::Failure, false));
        assert!(cond(
            Some("success() || failure()"),
            JobStatus::Failure,
            false
        ));
        assert!(cond(
            Some("( success() && !cancelled() ) || always()"),
            JobStatus::Failure,
            true
        ));
        // Unsupported atoms fail loudly rather than guessing.
        assert!(eval_condition(
            Some("github.event_name == 'push'"),
            JobStatus::Success,
            false
        )
        .is_err());
    }

    fn absorb_str(text: &str) -> Vec<(String, String)> {
        absorb_str_as(text, false)
    }

    fn absorb_str_as(text: &str, for_output: bool) -> Vec<(String, String)> {
        let dir = std::env::temp_dir().join(format!(
            "bupha-absorb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let f = dir.join("env");
        std::fs::write(&f, text).unwrap();
        let mut out = Vec::new();
        absorb_file(&f, for_output, |k, v| out.push((k, v)));
        let _ = std::fs::remove_dir_all(&dir);
        out
    }

    #[test]
    fn absorb_file_parses_name_value_and_skips_noise() {
        let entries = absorb_str(
            "# comment\n\
             FOO=bar\n\
             EMPTY=\n\
             WITH_EQUALS=a=b=c\n\
             NOT A NAME=x\n\
             1BAD=x\n",
        );
        assert_eq!(
            entries,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("EMPTY".to_string(), String::new()),
                ("WITH_EQUALS".to_string(), "a=b=c".to_string()),
            ]
        );
    }

    #[test]
    fn absorb_file_parses_multiline_heredocs() {
        let entries = absorb_str(
            "MSG<<EOF\nline one\nline two\nEOF\n\
             QUOTED<<'END'\nraw $NOT_EXPANDED\nEND\n\
             AFTER=still-parsed\n",
        );
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].0, "MSG");
        assert_eq!(entries[0].1, "line one\nline two\n");
        assert_eq!(entries[1].0, "QUOTED");
        assert_eq!(entries[1].1, "raw $NOT_EXPANDED\n");
        assert_eq!(
            entries[2],
            ("AFTER".to_string(), "still-parsed".to_string())
        );
    }

    #[test]
    fn absorb_file_last_write_wins() {
        // `>>` appends: two entries for one name — the sink folds them
        // into a map and the later wins, the semantics GitHub
        // documents for env files.
        let mut folded = BTreeMap::new();
        let entries = absorb_str("K=first\nK=second\n");
        for (k, v) in entries {
            folded.insert(k, v);
        }
        assert_eq!(folded["K"], "second");
    }

    #[test]
    fn absorb_file_output_names_allow_hyphens_env_names_do_not() {
        let text = "my-output=ok\nbad-name=x\n";
        let env_entries = absorb_str(text);
        assert!(env_entries.is_empty());
        let out_entries = absorb_str_as(text, true);
        assert_eq!(
            out_entries,
            vec![
                ("my-output".to_string(), "ok".to_string()),
                ("bad-name".to_string(), "x".to_string()),
            ]
        );
    }

    /// Scratch + JobEnv pair for exercising absorb_env_files directly.
    fn scratch_and_env() -> (Scratch, JobEnv, PathBuf) {
        let root = std::env::temp_dir().join(format!(
            "bupha-path-order-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let ws = root.join("job");
        std::fs::create_dir_all(&ws).unwrap();
        let job = JobMessage::decode(
            r#"{
            "messageType": "PipelineAgentJobRequest",
            "jobId": "path-order", "jobDisplayName": "x", "jobName": "x",
            "requestId": 1, "billingOwnerId": "o",
            "plan": {"planId": "p"}, "timeline": {"id": "t"},
            "contextData": {"t": 2, "d": []},
            "variables": {}, "resources": {"endpoints": []},
            "steps": []
        }"#,
        )
        .unwrap();
        let scratch = Scratch::new(&ws, &job).unwrap();
        let env = JobEnv {
            vars: BTreeMap::new(),
            path_dirs: Vec::new(),
            outputs: BTreeMap::new(),
        };
        (scratch, env, root)
    }

    fn path_prefix(env: &JobEnv) -> Vec<String> {
        let path = env
            .for_step()
            .into_iter()
            .find(|(k, _)| k == "PATH")
            .map(|(_, v)| v)
            .unwrap_or_default();
        path.split(':')
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect()
    }

    #[test]
    fn github_path_prepends_most_recent_first() {
        let (scratch, mut env, root) = scratch_and_env();
        // Step 1 writes two dirs — the LAST line takes precedence.
        std::fs::write(scratch.path_file(), "/step1-a\n/step1-b\n").unwrap();
        absorb_env_files(&scratch, &mut env, "s1");
        // Step 2 writes one dir — it outranks everything from s1.
        std::fs::write(scratch.path_file(), "/step2-a\n").unwrap();
        absorb_env_files(&scratch, &mut env, "s2");

        let dirs = path_prefix(&env);
        assert_eq!(dirs[..3], ["/step2-a", "/step1-b", "/step1-a"]);
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn github_output_lands_in_step_outputs() {
        let (scratch, mut env, root) = scratch_and_env();
        std::fs::write(
            scratch.output_file(),
            "answer=42\nmulti<<EOF\nhello\nworld\nEOF\n",
        )
        .unwrap();
        absorb_env_files(&scratch, &mut env, "s1");
        let outs = &env.outputs["s1"];
        assert_eq!(outs["answer"], "42");
        assert_eq!(outs["multi"], "hello\nworld\n");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Two script steps: the first exports through `$GITHUB_ENV`, the
    /// second must see it — exercises scratch wiring + env absorb.
    #[test]
    fn execute_runs_script_steps_and_env_files() {
        let job = JobMessage::decode(r#"{
            "messageType": "PipelineAgentJobRequest",
            "jobId": "exec-test",
            "jobDisplayName": "exec test",
            "jobName": "exec",
            "requestId": 1,
            "billingOwnerId": "o",
            "plan": {"planId": "p"},
            "timeline": {"id": "t"},
            "contextData": {"t": 2, "d": []},
            "variables": {},
            "resources": {"endpoints": []},
            "environmentVariables": {"t": 2, "d": [
                {"k": "BASE_VAR", "v": "base"}
            ]},
            "steps": [
                {
                    "type": "action", "id": "s1", "name": "run",
                    "displayNameToken": {"type": 0, "lit": "export"},
                    "reference": {"type": "script"},
                    "inputs": {"type": 2, "map": [
                        {"Key": {"type": 0, "lit": "script"},
                         "Value": {"type": 0, "lit": "echo EXPORTED=from-env >> \"$GITHUB_ENV\"\necho \"out-1=yep\" >> \"$GITHUB_OUTPUT\""}},
                        {"Key": {"type": 0, "lit": "shell"},
                         "Value": {"type": 0, "lit": "sh"}}
                    ]}
                },
                {
                    "type": "action", "id": "s2", "name": "run",
                    "displayNameToken": {"type": 0, "lit": "check"},
                    "reference": {"type": "script"},
                    "inputs": {"type": 2, "map": [
                        {"Key": {"type": 0, "lit": "script"},
                         "Value": {"type": 0, "lit": "echo \"got=$EXPORTED base=$BASE_VAR\"\nexit 0"}},
                        {"Key": {"type": 0, "lit": "shell"},
                         "Value": {"type": 0, "lit": "sh"}}
                    ]}
                }
            ]
        }"#)
        .unwrap();

        let ws = std::env::temp_dir().join(format!(
            "bupha-gha-runner-exec-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&ws).unwrap();
        let runner = JobRunner::new();
        let mut progress = NullProgress;
        let outcome = execute(&job, &ws, &runner, &mut progress);
        let _ = std::fs::remove_dir_all(ws.parent().unwrap_or(&ws));

        assert_eq!(outcome.conclusion, Outcome::Succeeded);
        assert_eq!(outcome.steps.len(), 2);
        assert!(outcome
            .steps
            .iter()
            .all(|s| s.conclusion == Some("succeeded")));
        assert!(outcome.job_log.contains("got=from-env base=base"));
        assert!(outcome.step_logs.contains_key("s1"));
        // The `>> $GITHUB_OUTPUT` line landed as a step output.
        assert_eq!(outcome.outputs["s1"]["out-1"], "yep");
    }

    /// Multiline `<<EOF` env writes, `$GITHUB_PATH` prepends and
    /// `$GITHUB_OUTPUT` all survive into the next step.
    #[test]
    fn execute_env_files_heredoc_path_and_outputs() {
        let job = JobMessage::decode(r#"{
            "messageType": "PipelineAgentJobRequest",
            "jobId": "exec-files",
            "jobDisplayName": "exec files", "jobName": "exec",
            "requestId": 1, "billingOwnerId": "o",
            "plan": {"planId": "p"}, "timeline": {"id": "t"},
            "contextData": {"t": 2, "d": []},
            "variables": {}, "resources": {"endpoints": []},
            "environmentVariables": {"t": 2, "d": []},
            "steps": [
                {
                    "type": "action", "id": "s1", "name": "run",
                    "displayNameToken": {"type": 0, "lit": "write files"},
                    "reference": {"type": "script"},
                    "inputs": {"type": 2, "map": [
                        {"Key": {"type": 0, "lit": "script"},
                         "Value": {"type": 0, "lit": "mkdir -p \"$RUNNER_TEMP/bin\"\nprintf '#!/bin/sh\\necho from-mytool\\n' > \"$RUNNER_TEMP/bin/mytool\"\nchmod +x \"$RUNNER_TEMP/bin/mytool\"\necho \"$RUNNER_TEMP/bin\" >> \"$GITHUB_PATH\"\ncat >> \"$GITHUB_ENV\" <<'MYEOF'\nMULTI<<INNER\nhello\nworld\nINNER\nMYEOF\necho \"ver=1.2.3\" >> \"$GITHUB_OUTPUT\""}},
                        {"Key": {"type": 0, "lit": "shell"},
                         "Value": {"type": 0, "lit": "sh"}}
                    ]}
                },
                {
                    "type": "action", "id": "s2", "name": "run",
                    "displayNameToken": {"type": 0, "lit": "verify"},
                    "reference": {"type": "script"},
                    "inputs": {"type": 2, "map": [
                        {"Key": {"type": 0, "lit": "script"},
                         "Value": {"type": 0, "lit": "echo \"tool says: $(mytool)\"\necho \"multi=$MULTI\"\ncase \"$PATH\" in *bin:*) echo \"path-ok\";; *) echo \"path-bad\"; exit 1;; esac"}},
                        {"Key": {"type": 0, "lit": "shell"},
                         "Value": {"type": 0, "lit": "sh"}}
                    ]}
                }
            ]
        }"#)
        .unwrap();

        let ws = std::env::temp_dir().join(format!(
            "bupha-gha-runner-exec3-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&ws).unwrap();
        let runner = JobRunner::new();
        let mut progress = NullProgress;
        let outcome = execute(&job, &ws, &runner, &mut progress);
        let _ = std::fs::remove_dir_all(ws.parent().unwrap_or(&ws));

        assert_eq!(outcome.conclusion, Outcome::Succeeded);
        assert!(
            outcome.job_log.contains("tool says: from-mytool"),
            "log: {}",
            outcome.job_log
        );
        assert!(outcome.job_log.contains("multi=hello\nworld"));
        assert!(outcome.job_log.contains("path-ok"));
        assert_eq!(outcome.outputs["s1"]["ver"], "1.2.3");
    }

    /// `jobOutputs` templates evaluate against collected step
    /// outputs, the pre-filled `needs` context and env — and land on
    /// `JobOutcome.job_outputs` for `completejob`.
    #[test]
    fn execute_evaluates_job_outputs() {
        let job = JobMessage::decode(r#"{
            "messageType": "PipelineAgentJobRequest",
            "jobId": "exec-outputs",
            "jobDisplayName": "x", "jobName": "x",
            "requestId": 1, "billingOwnerId": "o",
            "plan": {"planId": "p"}, "timeline": {"id": "t"},
            "contextData": {"t": 2, "d": [
                {"k": "needs", "v": {"t": 2, "d": [
                    {"k": "up", "v": {"t": 2, "d": [
                        {"k": "outputs", "v": {"t": 2, "d": [
                            {"k": "token", "v": "sekret"}
                        ]}}
                    ]}}
                ]}}
            ]},
            "variables": {}, "resources": {"endpoints": []},
            "jobOutputs": {"type": 2, "map": [
                {"Key": {"type": 0, "lit": "answer"},
                 "Value": {"type": 3, "expr": "steps.s1.outputs.answer"}},
                {"Key": {"type": 0, "lit": "from-need"},
                 "Value": {"type": 3, "expr": "needs.up.outputs.token"}},
                {"Key": {"type": 0, "lit": "env-val"},
                 "Value": {"type": 3, "expr": "env.MY_VAR"}},
                {"Key": {"type": 0, "lit": "static"},
                 "Value": {"type": 0, "lit": "plain"}},
                {"Key": {"type": 0, "lit": "unsupported"},
                 "Value": {"type": 3, "expr": "matrix.nope"}}
            ]},
            "steps": [{
                "type": "action", "id": "s1", "name": "run",
                "displayNameToken": {"type": 0, "lit": "write"},
                "reference": {"type": "script"},
                "inputs": {"type": 2, "map": [
                    {"Key": {"type": 0, "lit": "script"},
                     "Value": {"type": 0, "lit": "echo \"MY_VAR=set-by-step\" >> \"$GITHUB_ENV\"\necho \"answer=42\" >> \"$GITHUB_OUTPUT\"\nexit 0"}},
                    {"Key": {"type": 0, "lit": "shell"},
                     "Value": {"type": 0, "lit": "sh"}}
                ]}
            }]
        }"#)
        .unwrap();
        assert_eq!(job.job_outputs["answer"], "${{ steps.s1.outputs.answer }}");

        let ws = std::env::temp_dir().join(format!(
            "bupha-gha-runner-exec-out-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&ws).unwrap();
        let runner = JobRunner::new();
        let mut progress = NullProgress;
        let outcome = execute(&job, &ws, &runner, &mut progress);
        let _ = std::fs::remove_dir_all(ws.parent().unwrap_or(&ws));

        assert_eq!(outcome.conclusion, Outcome::Succeeded);
        assert_eq!(outcome.job_outputs["answer"], "42");
        assert_eq!(outcome.job_outputs["from-need"], "sekret");
        // env.* sees the GITHUB_ENV write folded in after the step.
        assert_eq!(outcome.job_outputs["env-val"], "set-by-step");
        assert_eq!(outcome.job_outputs["static"], "plain");
        // Unsupported exprs leave the output unset, not wrong.
        assert!(!outcome.job_outputs.contains_key("unsupported"));
    }

    #[test]
    fn execute_fails_unsupported_steps_loudly() {
        let job = JobMessage::decode(
            r#"{
            "messageType": "PipelineAgentJobRequest",
            "jobId": "exec-unsupported",
            "jobDisplayName": "x", "jobName": "x",
            "requestId": 1, "billingOwnerId": "o",
            "plan": {"planId": "p"}, "timeline": {"id": "t"},
            "contextData": {"t": 2, "d": []},
            "variables": {}, "resources": {"endpoints": []},
            "steps": [{
                "type": "action", "id": "s1", "name": "__run",
                "displayNameToken": {"type": 0, "lit": "node action"},
                "reference": {"type": "repository", "name": "actions/setup-node",
                              "ref": "v4", "repositoryType": "GitHub"},
                "inputs": {"type": 2, "map": []}
            }]
        }"#,
        )
        .unwrap();
        let ws = std::env::temp_dir().join(format!(
            "bupha-gha-runner-exec2-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&ws).unwrap();
        let runner = JobRunner::new();
        let mut progress = NullProgress;
        let outcome = execute(&job, &ws, &runner, &mut progress);
        let _ = std::fs::remove_dir_all(ws.parent().unwrap_or(&ws));

        assert_eq!(outcome.conclusion, Outcome::Failed);
        assert!(outcome
            .job_log
            .contains("unsupported action `actions/setup-node@v4`"));
    }
}
