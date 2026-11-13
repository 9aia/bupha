//! plan — the pure workflow core: `.github/workflows/*.yml` text in,
//! validated [`Job`] plans out. No I/O — the runner hands in YAML
//! strings and an [`Event`] (ref + changed files); every construct
//! outside the v1 subset is an explicit error naming the YAML key.

use serde_norway::Value;
use std::collections::{HashMap, HashSet};

/// The push event a workflow is selected against.
pub struct Event<'a> {
    /// Full git ref ("refs/heads/main"); "" when the source can't
    /// say — `branches` filters then fail loudly rather than guess.
    pub git_ref: &'a str,
    /// Repo-relative paths changed by the push (v1: tip-commit
    /// diff — see the crate README for the honest boundary).
    pub changed_files: &'a [String],
}

/// GitHub-side facts `${{ }}` expressions can reference.
#[derive(Clone, Copy)]
pub struct GhaContext<'a> {
    pub sha: &'a str,
    pub git_ref: &'a str,
    /// "owner/name" (no ":job" suffix).
    pub repository: &'a str,
    /// Always "push" today.
    pub event_name: &'a str,
}

/// A parsed + validated workflow file.
pub struct Workflow {
    /// `name:` or the file name.
    pub name: String,
    env: Vec<(String, String)>,
    defaults: Defaults,
    filters: PushFilters,
    jobs: Vec<RawJob>,
}

#[derive(Default)]
struct Defaults {
    working_directory: Option<String>,
    shell: Option<String>,
}

#[derive(Default)]
struct PushFilters {
    branches: Option<Vec<String>>,
    branches_ignore: Option<Vec<String>>,
    paths: Option<Vec<String>>,
    paths_ignore: Option<Vec<String>>,
    /// Set when the workflow has no `push` trigger at all.
    never: bool,
}

struct RawJob {
    id: String,
    name: Option<String>,
    /// Raw `if:` text — parsed + folded in `build_job`.
    cond: Option<String>,
    needs: Vec<String>,
    runs_on: Vec<String>,
    env: Vec<(String, String)>,
    defaults: Defaults,
    matrix: Vec<(String, Vec<String>)>,
    /// `strategy.fail-fast` (GHA default: true when a matrix
    /// exists) — later legs of the same job id are cancelled after
    /// the first failure.
    fail_fast: bool,
    /// `strategy.max-parallel` — recorded; execution stays
    /// sequential (parallel matrix is a documented deferral).
    max_parallel: Option<u32>,
    /// `timeout-minutes` — enforced by the renderer (job watchdog /
    /// step `timeout` wrapper), stored as whole seconds.
    timeout_seconds: Option<u64>,
    /// `outputs:` — output name → value template (usually a single
    /// `${{ steps.<id>.outputs.<name> }}`). Interpolated at expand;
    /// `needs.*`/`steps.*` refs stay deferred for the renderer.
    outputs: Vec<(String, String)>,
    steps: Vec<RawStep>,
}

enum RawStep {
    Run {
        /// `id:` — the handle `steps.<id>.outputs.*` references.
        /// `None` steps are unreachable from expressions.
        id: Option<String>,
        /// Raw `if:` text.
        cond: Option<String>,
        /// `timeout-minutes` in whole seconds.
        timeout_seconds: Option<u64>,
        shell: Option<String>,
        working_directory: Option<String>,
        env: Vec<(String, String)>,
        script: String,
    },
    /// `uses: actions/checkout` — verification no-op (the worktree
    /// is already at the pushed sha).
    Checkout {
        id: Option<String>,
        cond: Option<String>,
    },
    /// `uses:` lowered to a synthesized `run:` step — raw `with:`
    /// pairs (each value a list: scalars are one entry, YAML lists
    /// flatten), interpolated in `build_step`.
    Script {
        kind: UsesScript,
        id: Option<String>,
        cond: Option<String>,
        timeout_seconds: Option<u64>,
        with: Vec<(String, Vec<String>)>,
    },
}

/// One runnable job after matrix fan-out, `${{ }}` interpolation and
/// `needs` resolution. Indices in `needs` point into the slice this
/// job came from (the workflow's expanded, topo-ordered list).
pub struct Job {
    /// Workflow display name (for logs).
    pub workflow: String,
    /// The job's YAML id — the handle `needs.<id>.outputs.*`
    /// expressions reference (not `name:`/`label`).
    pub id: String,
    /// `name:` or the job id.
    pub label: String,
    /// Labels GitHub would schedule on (recorded; execution is
    /// always local).
    pub runs_on: Vec<String>,
    /// Indices of jobs that must pass first (same-workflow only).
    pub needs: Vec<usize>,
    /// Workflow + job env merged (last wins), values interpolated.
    pub env: Vec<(String, String)>,
    pub working_directory: Option<String>,
    pub shell: Option<String>,
    /// `outputs:` — name → interpolated template. `needs.*`/`steps.*`
    /// refs inside survive as literal `${{ … }}` for the renderer,
    /// which resolves them against collected step outputs once the
    /// job has run.
    pub outputs: Vec<(String, String)>,
    /// `if:` — parsed + folded (env/github/matrix resolved, needs/
    /// steps refs deferred). `None` → the implicit needs gate.
    pub cond: Option<Cond>,
    /// `timeout-minutes` in whole seconds — the renderer enforces
    /// it with a watchdog around the job body.
    pub timeout_seconds: Option<u64>,
    /// `strategy.fail-fast` (default true with a matrix): after a
    /// leg of this job id fails, remaining legs are cancelled.
    pub fail_fast: bool,
    /// `strategy.max-parallel` — honored as a logged no-op; legs
    /// still run sequentially.
    pub max_parallel: Option<u32>,
    /// Matrix bindings of this leg (`os=ubuntu,node=18`, declaration
    /// order) — `None` for non-matrix jobs. Distinguishes legs of
    /// the same `id` for per-job status contexts.
    pub leg: Option<String>,
    pub steps: Vec<Step>,
}

pub enum Step {
    Run(RunStep),
    Checkout {
        id: Option<String>,
        cond: Option<Cond>,
    },
}

pub struct RunStep {
    /// `id:` — `steps.<id>.outputs.*` handle; `None` when the step
    /// declared no id (its `$GITHUB_OUTPUT` writes stay unreferenced).
    pub id: Option<String>,
    /// `if:` — same semantics as the job-level condition.
    pub cond: Option<Cond>,
    /// `timeout-minutes` in whole seconds — rendered as a `timeout`
    /// wrapper around the step's shell.
    pub timeout_seconds: Option<u64>,
    pub shell: Option<String>,
    pub working_directory: Option<String>,
    /// Step-level env only (job env is exported by the job block).
    pub env: Vec<(String, String)>,
    pub script: String,
}

// ------------------------------------------------------------------
// `if:` expressions — parsed AST, literals folded at expand time
// ------------------------------------------------------------------

/// A parsed `if:` condition. `env.*`/`github.*`/`matrix.*` fold to
/// [`Cond::Lit`] during expand; `needs.*`/`steps.*` stay deferred —
/// the renderer turns them into lookups against the run's status
/// arrays and output stores.
#[derive(Debug)]
pub enum Cond {
    /// A folded scalar — truthiness applies (`""`, `"0"`, `"false"`,
    /// `"null"` are falsy, matching GHA's loose rules closely
    /// enough for CI conditions).
    Lit(String),
    /// A runtime-resolved reference.
    Dyn(DynRef),
    /// A builtin function call (name lowercased).
    Call {
        name: String,
        args: Vec<Cond>,
    },
    Bin {
        op: BinOp,
        l: Box<Cond>,
        r: Box<Cond>,
    },
    Not(Box<Cond>),
}

/// The deferred half of a condition — resolved by the renderer
/// against the run's `jobstatus/`+`jobresult/`+`stepconcl/`+
/// `stepouts/`+`jobouts/` stores under `$BUPHA_GHA_TMP`.
#[derive(Debug)]
pub enum DynRef {
    /// `needs.<job>.outputs.<name>` — the producer job's published
    /// output (last matrix leg wins, like job outputs generally).
    NeedOutput { job: String, name: String },
    /// `steps.<id>.outputs.<name>` — an earlier step's
    /// `$GITHUB_OUTPUT` entry in the same job.
    StepOutput { step: String, name: String },
    /// `needs.<job>.result` — `success`/`failure`/`cancelled`/
    /// `skipped` aggregated over the job's expanded legs.
    NeedResult { job: String },
    /// `steps.<id>.conclusion` — `success`/`skipped` (`failure` is
    /// unobservable: a failed step ends the job first).
    StepConclusion { step: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Eq,
    Ne,
    And,
    Or,
}

impl Cond {
    /// Does the tree contain a status-check function anywhere?
    /// GHA implicitly ANDs `success()` onto an `if:` that lacks
    /// one — the parser applies that wrap.
    fn has_status_fn(&self) -> bool {
        match self {
            Cond::Lit(_) | Cond::Dyn(_) => false,
            Cond::Call { name, .. } => matches!(
                name.as_str(),
                "success" | "failure" | "cancelled" | "always"
            ),
            Cond::Bin { l, r, .. } => l.has_status_fn() || r.has_status_fn(),
            Cond::Not(inner) => inner.has_status_fn(),
        }
    }
}

// ------------------------------------------------------------------
// YAML → Value helpers
// ------------------------------------------------------------------

fn as_map<'a>(v: &'a Value, what: &str) -> Result<&'a serde_norway::Mapping, String> {
    v.as_mapping()
        .ok_or_else(|| format!("{what}: expected a mapping, got {}", kind(v)))
}

fn get<'a>(m: &'a serde_norway::Mapping, key: &str) -> Option<&'a Value> {
    m.get(Value::String(key.to_owned()))
}

fn kind(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Sequence(_) => "sequence",
        Value::Mapping(_) => "mapping",
        Value::Tagged(_) => "tagged value",
    }
}

fn scalar(v: &Value, what: &str) -> Result<String, String> {
    match v {
        Value::String(s) => Ok(s.clone()),
        Value::Number(n) => Ok(n.to_string()),
        Value::Bool(b) => Ok(b.to_string()),
        _ => Err(format!("{what}: expected a scalar, got {}", kind(v))),
    }
}

/// Reject keys outside the v1 allowlist — unsupported syntax is a
/// hard error, never a silent skip. `unsupported` names get a
/// clearer message than truly unknown keys.
fn check_keys(
    m: &serde_norway::Mapping,
    allowed: &[&str],
    unsupported: &[&str],
    what: &str,
) -> Result<(), String> {
    for (k, _) in m {
        let key = k.as_str().unwrap_or("");
        if unsupported.contains(&key) {
            return Err(format!(
                "{what}: unsupported key `{key}` (outside the gha v1 subset)"
            ));
        }
        if !allowed.contains(&key) {
            return Err(format!("{what}: unknown key `{key}`"));
        }
    }
    Ok(())
}

fn string_list(v: &Value, what: &str) -> Result<Vec<String>, String> {
    match v {
        Value::String(s) => Ok(vec![s.clone()]),
        Value::Sequence(seq) => {
            if seq.is_empty() {
                return Err(format!("{what}: empty list"));
            }
            seq.iter()
                .map(|i| match i {
                    Value::String(s) => Ok(s.clone()),
                    _ => Err(format!("{what}: entries must be strings")),
                })
                .collect()
        }
        _ => Err(format!("{what}: expected a string or list of strings")),
    }
}

fn env_map(m: &serde_norway::Mapping, what: &str) -> Result<Vec<(String, String)>, String> {
    let mut out = Vec::with_capacity(m.len());
    for (k, v) in m {
        let key = k
            .as_str()
            .ok_or_else(|| format!("{what}: env names must be strings"))?;
        let valid = key
            .chars()
            .next()
            .map(|c| c.is_ascii_alphabetic() || c == '_')
            .unwrap_or(false)
            && key.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
        if !valid {
            return Err(format!("{what}: invalid env name `{key}`"));
        }
        // `KEY:` with no value is YAML null — GitHub coerces it to
        // the empty string, so do the same.
        let value = match v {
            Value::Null => String::new(),
            _ => scalar(v, &format!("{what}.{key}"))?,
        };
        out.push((key.to_string(), value));
    }
    Ok(out)
}

// ------------------------------------------------------------------
// on.push filters
// ------------------------------------------------------------------

fn parse_push_filters(v: &Value, what: &str) -> Result<PushFilters, String> {
    let m = match v {
        Value::Null => return Ok(PushFilters::default()),
        Value::Mapping(m) => m,
        _ => return Err(format!("{what}: `on.push` must be a mapping or null")),
    };
    check_keys(
        m,
        &["branches", "branches-ignore", "paths", "paths-ignore"],
        &["tags", "types", "workflows"],
        what,
    )?;
    let mut f = PushFilters::default();
    for (k, v) in m {
        let key = k.as_str().unwrap_or("");
        let list = string_list(v, &format!("{what}.{key}"))?;
        match key {
            "branches" => f.branches = Some(list),
            "branches-ignore" => f.branches_ignore = Some(list),
            "paths" => f.paths = Some(list),
            "paths-ignore" => f.paths_ignore = Some(list),
            _ => unreachable!(),
        }
    }
    Ok(f)
}

/// `on:` may be a string, a list, or a map of trigger → config.
/// Triggers other than `push` never fire here — ignored, not errors.
fn parse_on(v: Option<&Value>, what: &str) -> Result<PushFilters, String> {
    let Some(v) = v else {
        return Err(format!("{what}: missing `on`"));
    };
    match v {
        Value::String(s) => Ok(if s == "push" {
            PushFilters::default()
        } else {
            PushFilters {
                never: true,
                ..PushFilters::default()
            }
        }),
        Value::Sequence(seq) => {
            let mut saw_push = false;
            for item in seq {
                match item.as_str() {
                    Some("push") => saw_push = true,
                    Some(_) => {}
                    None => return Err(format!("{what}: `on` entries must be strings")),
                }
            }
            Ok(PushFilters {
                never: !saw_push,
                ..PushFilters::default()
            })
        }
        Value::Mapping(m) => {
            let mut filters = PushFilters {
                never: true,
                ..PushFilters::default()
            };
            for (k, v) in m {
                if k.as_str() == Some("push") {
                    filters = parse_push_filters(v, &format!("{what}.on.push"))?;
                }
            }
            Ok(filters)
        }
        _ => Err(format!("{what}: unsupported `on` shape ({})", kind(v))),
    }
}

// ------------------------------------------------------------------
// glob matching — `*`/`?` stay inside a path segment, `**` crosses `/`
// ------------------------------------------------------------------

fn glob_match(pat: &[u8], text: &[u8]) -> bool {
    if pat.is_empty() {
        return text.is_empty();
    }
    if pat.starts_with(b"**") {
        return (0..=text.len()).any(|skip| glob_match(&pat[2..], &text[skip..]));
    }
    if text.is_empty() {
        return false;
    }
    match pat[0] {
        b'*' => (0..=text.len())
            .take_while(|&skip| !text[..skip].contains(&b'/'))
            .any(|skip| glob_match(&pat[1..], &text[skip..])),
        b'?' => text[0] != b'/' && glob_match(&pat[1..], &text[1..]),
        c => text[0] == c && glob_match(&pat[1..], &text[1..]),
    }
}

fn any_glob(patterns: &[String], text: &str) -> bool {
    patterns
        .iter()
        .any(|p| glob_match(p.as_bytes(), text.as_bytes()))
}

fn short_ref(git_ref: &str) -> &str {
    git_ref
        .strip_prefix("refs/heads/")
        .or_else(|| git_ref.strip_prefix("refs/tags/"))
        .unwrap_or(git_ref)
}

impl Workflow {
    /// Does this workflow's `on.push` select the event?
    pub fn matches_push(&self, ev: &Event) -> Result<bool, String> {
        if self.filters.never {
            return Ok(false);
        }
        let f = &self.filters;
        let short = short_ref(ev.git_ref);
        if let Some(pats) = &f.branches {
            if ev.git_ref.is_empty() {
                return Err("on.push.branches: cannot evaluate — event ref is unknown".into());
            }
            if !any_glob(pats, short) {
                return Ok(false);
            }
        }
        if let Some(pats) = &f.branches_ignore {
            if ev.git_ref.is_empty() {
                return Err(
                    "on.push.branches-ignore: cannot evaluate — event ref is unknown".into(),
                );
            }
            if any_glob(pats, short) {
                return Ok(false);
            }
        }
        if let Some(pats) = &f.paths {
            if !ev.changed_files.iter().any(|f| any_glob(pats, f)) {
                return Ok(false);
            }
        }
        if let Some(pats) = &f.paths_ignore {
            // Runs unless EVERY changed file matches an ignore
            // pattern; an empty diff has nothing to ignore.
            let all_ignored =
                !ev.changed_files.is_empty() && ev.changed_files.iter().all(|f| any_glob(pats, f));
            if all_ignored {
                return Ok(false);
            }
        }
        Ok(true)
    }

    /// Fan out matrix combos, interpolate `${{ }}`, resolve `needs`
    /// and topo-order. `ctx` supplies the github.* facts.
    pub fn expand(&self, ctx: GhaContext) -> Result<Vec<Job>, String> {
        // Matrix fan-out, declaration order preserved.
        let mut flat: Vec<(usize, Vec<(String, String)>)> = Vec::new();
        for (ji, j) in self.jobs.iter().enumerate() {
            for combo in matrix_combos(&j.matrix, &format!("jobs.{}", j.id))? {
                flat.push((ji, combo));
            }
        }
        // needs id → every expanded index of that job.
        let mut by_id: HashMap<&str, Vec<usize>> = HashMap::new();
        for (idx, (ji, _)) in flat.iter().enumerate() {
            by_id
                .entry(self.jobs[*ji].id.as_str())
                .or_default()
                .push(idx);
        }
        let mut jobs = Vec::with_capacity(flat.len());
        for (ji, combo) in &flat {
            jobs.push(self.build_job(&self.jobs[*ji], combo, &by_id, ctx)?);
        }
        topo_order(jobs)
    }

    fn build_job(
        &self,
        j: &RawJob,
        combo: &[(String, String)],
        by_id: &HashMap<&str, Vec<usize>>,
        ctx: GhaContext,
    ) -> Result<Job, String> {
        let what = format!("jobs.{}", j.id);
        let mut needs: Vec<usize> = j
            .needs
            .iter()
            .flat_map(|n| by_id.get(n.as_str()).cloned().unwrap_or_default())
            .collect();
        needs.sort_unstable();
        needs.dedup();

        // env.* is unavailable while defining env — github.*/matrix.*
        // only. Workflow env merges under job env.
        let no_env = Eval {
            ctx,
            matrix: combo,
            env: None,
        };
        let wf_env = interpolate_pairs(&self.env, &no_env, &format!("{what}.env"))?;
        let job_env = merge_env(
            &wf_env,
            &interpolate_pairs(&j.env, &no_env, &format!("{what}.env"))?,
        );
        let eval = Eval {
            ctx,
            matrix: combo,
            env: Some(&job_env),
        };

        // `if:` evaluates in job context — env.* sees the merged
        // workflow+job env (step env doesn't exist yet), github.*/
        // matrix.* fold, needs.*/steps.* defer to the renderer.
        let cond = j
            .cond
            .as_deref()
            .map(|raw| parse_cond(raw, &eval, &format!("{what}.if")))
            .transpose()?;
        let mut steps = Vec::with_capacity(j.steps.len());
        for (si, s) in j.steps.iter().enumerate() {
            steps.push(build_step(
                s,
                &eval,
                &job_env,
                ctx,
                &format!("{what}.steps[{si}]"),
            )?);
        }
        let defaults = Defaults {
            working_directory: pick(
                &j.defaults.working_directory,
                &self.defaults.working_directory,
            )
            .map(|d| interpolate(d, &eval, &format!("{what}.defaults.run.working-directory")))
            .transpose()?,
            shell: pick(&j.defaults.shell, &self.defaults.shell)
                .map(|s| interpolate(s, &eval, &format!("{what}.defaults.run.shell")))
                .transpose()?,
        };
        // `outputs:` values evaluate in job context — env.* resolves
        // now; needs.*/steps.* stay deferred for the renderer.
        let outputs = j
            .outputs
            .iter()
            .map(|(k, v)| {
                Ok((
                    k.clone(),
                    interpolate(v, &eval, &format!("{what}.outputs.{k}"))?,
                ))
            })
            .collect::<Result<Vec<_>, String>>()?;
        Ok(Job {
            workflow: self.name.clone(),
            id: j.id.clone(),
            label: match &j.name {
                Some(n) => interpolate(n, &eval, &format!("{what}.name"))?,
                None => j.id.clone(),
            },
            runs_on: j
                .runs_on
                .iter()
                .map(|r| interpolate(r, &eval, &format!("{what}.runs-on")))
                .collect::<Result<_, _>>()?,
            needs,
            env: job_env,
            working_directory: defaults.working_directory,
            shell: defaults.shell,
            outputs,
            cond,
            timeout_seconds: j.timeout_seconds,
            fail_fast: j.fail_fast,
            max_parallel: j.max_parallel,
            leg: (!combo.is_empty()).then(|| {
                combo
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect::<Vec<_>>()
                    .join(",")
            }),
            steps,
        })
    }
}

fn pick<'a>(job: &'a Option<String>, wf: &'a Option<String>) -> Option<&'a String> {
    job.as_ref().or(wf.as_ref())
}

/// Last-wins merge of two env lists.
fn merge_env(base: &[(String, String)], over: &[(String, String)]) -> Vec<(String, String)> {
    let mut out = base.to_vec();
    for (k, v) in over {
        match out.iter_mut().find(|(ek, _)| ek == k) {
            Some(slot) => slot.1 = v.clone(),
            None => out.push((k.clone(), v.clone())),
        }
    }
    out
}

/// Interpolate every value in an env list (env.* unavailable).
fn interpolate_pairs(
    raw: &[(String, String)],
    eval: &Eval,
    what: &str,
) -> Result<Vec<(String, String)>, String> {
    raw.iter()
        .map(|(k, v)| Ok((k.clone(), interpolate(v, eval, &format!("{what}.{k}"))?)))
        .collect()
}

/// Interpolation context for one expansion — github facts plus the
/// matrix bindings and merged env visible at this point.
pub struct Eval<'a> {
    pub ctx: GhaContext<'a>,
    pub matrix: &'a [(String, String)],
    pub env: Option<&'a [(String, String)]>,
}

fn build_step(
    s: &RawStep,
    eval: &Eval,
    job_env: &[(String, String)],
    ctx: GhaContext,
    what: &str,
) -> Result<Step, String> {
    match s {
        RawStep::Checkout { id, cond } => Ok(Step::Checkout {
            id: id.clone(),
            cond: cond
                .as_deref()
                .map(|raw| parse_cond(raw, eval, &format!("{what}.if")))
                .transpose()?,
        }),
        RawStep::Script {
            kind,
            id,
            cond,
            timeout_seconds,
            with,
        } => {
            // Lower to a synthesized run step: interpolate the
            // `with:` inputs, then emit the action's bash. The
            // script itself contains no `${{ }}` — interpolation
            // already happened — so it goes through verbatim.
            let mut pairs = Vec::with_capacity(with.len());
            for (k, vals) in with {
                let mut out = Vec::with_capacity(vals.len());
                for v in vals {
                    out.push(interpolate(v, eval, &format!("{what}.with.{k}"))?);
                }
                pairs.push((k.clone(), out));
            }
            let script = match kind {
                UsesScript::RustToolchain => rust_toolchain_script(&pairs, what)?,
                UsesScript::UploadArtifact => upload_artifact_script(&pairs, what)?,
                UsesScript::DownloadArtifact => download_artifact_script(&pairs, what)?,
            };
            Ok(Step::Run(RunStep {
                id: id.clone(),
                cond: cond
                    .as_deref()
                    .map(|raw| parse_cond(raw, eval, &format!("{what}.if")))
                    .transpose()?,
                timeout_seconds: *timeout_seconds,
                shell: None,
                working_directory: None,
                env: Vec::new(),
                script,
            }))
        }
        RawStep::Run {
            id,
            cond,
            timeout_seconds,
            shell,
            working_directory,
            env,
            script,
        } => {
            // Step env merges over job env; its own pairs are what
            // the step block exports.
            let step_own = interpolate_pairs(env, eval, &format!("{what}.env"))?;
            let step_env = merge_env(job_env, &step_own);
            let step_eval = Eval {
                ctx,
                matrix: eval.matrix,
                env: Some(&step_env),
            };
            Ok(Step::Run(RunStep {
                id: id.clone(),
                cond: cond
                    .as_deref()
                    .map(|raw| parse_cond(raw, &step_eval, &format!("{what}.if")))
                    .transpose()?,
                timeout_seconds: *timeout_seconds,
                shell: shell
                    .as_ref()
                    .map(|s| interpolate(s, &step_eval, &format!("{what}.shell")))
                    .transpose()?,
                working_directory: working_directory
                    .as_ref()
                    .map(|w| interpolate(w, &step_eval, &format!("{what}.working-directory")))
                    .transpose()?,
                env: step_own,
                script: interpolate(script, &step_eval, &format!("{what}.run"))?,
            }))
        }
    }
}

/// `dtolnay/rust-toolchain` lowered to bash: install the toolchain
/// via rustup (minimal profile + requested components/targets),
/// then make it win for later steps — the toolchain's bin dir goes
/// on `$GITHUB_PATH` (last line front-most after absorb) and
/// `RUSTUP_TOOLCHAIN` on `$GITHUB_ENV` so rustup proxies agree.
/// With no `toolchain:` input the channel comes from
/// `rust-toolchain.toml`/`rust-toolchain` in the worktree, else
/// `stable` — mirroring the action.
fn rust_toolchain_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    let mut toolchain = None;
    let mut components = Vec::new();
    let mut targets = Vec::new();
    for (k, vals) in pairs {
        match k.as_str() {
            "toolchain" => {
                if vals.len() != 1 {
                    return Err(format!("{what}.with.toolchain: expected a single value"));
                }
                toolchain = Some(vals[0].clone());
            }
            "components" | "targets" => {
                let flat: Vec<String> = vals
                    .iter()
                    .flat_map(|v| {
                        v.split(',')
                            .map(str::trim)
                            .filter(|s| !s.is_empty())
                            .map(str::to_string)
                            .collect::<Vec<_>>()
                    })
                    .collect();
                if k == "components" {
                    components = flat;
                } else {
                    targets = flat;
                }
            }
            other => {
                return Err(format!(
                    "{what}.with.{other}: unsupported input for \
                     dtolnay/rust-toolchain — supported: toolchain, \
                     components, targets"
                ))
            }
        }
    }
    let tc = toolchain.unwrap_or_default();
    let mut script = String::new();
    script.push_str("set -e\n");
    script.push_str(&format!("tc={}\n", bash_sq(&tc)));
    script.push_str(
        "if [ -z \"$tc\" ]; then\n\
         \x20 if [ -f rust-toolchain.toml ]; then\n\
         \x20   tc=$(sed -n 's/.*channel[[:space:]]*=[[:space:]]*\"\\([^\"]*\\)\".*/\\1/p' rust-toolchain.toml | head -1)\n\
         \x20 elif [ -f rust-toolchain ]; then\n\
         \x20   tc=$(head -1 rust-toolchain | tr -d '[:space:]')\n\
         \x20 fi\n\
         \x20 tc=${tc:-stable}\n\
         fi\n\
         args=(toolchain install \"$tc\" --profile minimal)\n",
    );
    if !components.is_empty() {
        script.push_str(&format!(
            "args+=(--component {})\n",
            bash_sq(&components.join(","))
        ));
    }
    if !targets.is_empty() {
        script.push_str(&format!(
            "args+=(--target {})\n",
            bash_sq(&targets.join(","))
        ));
    }
    script.push_str(
        "rustup \"${args[@]}\"\n\
         tcbin=$(dirname \"$(rustup which --toolchain \"$tc\" cargo 2>/dev/null)\" 2>/dev/null || :)\n\
         if [ -n \"$tcbin\" ] && [ -d \"$tcbin\" ]; then echo \"$tcbin\" >> \"$GITHUB_PATH\"; fi\n\
         printf 'RUSTUP_TOOLCHAIN=%s\\n' \"$tc\" >> \"$GITHUB_ENV\"\n",
    );
    Ok(script)
}

/// Single-quote a literal for embedding in the synthesized bash —
/// same escaping rule as the renderer's `sq`.
fn bash_sq(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/// A `with:` input expected to be a single value.
fn with_one<'a>(
    pairs: &'a [(String, Vec<String>)],
    key: &str,
    what: &str,
) -> Result<Option<&'a str>, String> {
    match pairs.iter().find(|(k, _)| k == key) {
        None => Ok(None),
        Some((_, v)) if v.len() == 1 => Ok(Some(v[0].as_str())),
        Some(_) => Err(format!("{what}.with.{key}: expected a single value")),
    }
}

/// A `with:` boolean input (`true`/`false`, case-insensitive).
fn with_flag(
    pairs: &[(String, Vec<String>)],
    key: &str,
    what: &str,
) -> Result<Option<bool>, String> {
    with_one(pairs, key, what)?
        .map(|v| match v.to_ascii_lowercase().as_str() {
            "true" => Ok(true),
            "false" => Ok(false),
            _ => Err(format!(
                "{what}.with.{key}: expected true or false, got `{v}`"
            )),
        })
        .transpose()
}

/// Reject `with:` keys outside the action's allowlist.
fn check_with_keys(
    pairs: &[(String, Vec<String>)],
    allowed: &[&str],
    what: &str,
) -> Result<(), String> {
    for (k, _) in pairs {
        if !allowed.contains(&k.as_str()) {
            return Err(format!(
                "{what}.with.{k}: unsupported input — supported: {}",
                allowed.join(", ")
            ));
        }
    }
    Ok(())
}

/// Artifact names become filenames in the per-run store and path
/// components on download — apply GitHub's v4 charset rule plus
/// the `.`/`..` guard.
fn artifact_name_ok(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.chars().all(|c| {
            !c.is_control() && !matches!(c, '"' | ':' | '<' | '>' | '|' | '*' | '?' | '/' | '\\')
        })
}

/// `actions/upload-artifact` lowered to bash over the per-run
/// artifact store: `$BUPHA_GHA_TMP/artifacts/<name>.tar`, shared by
/// every job in the run — matching download-artifact's same-run
/// default. `path` entries are workspace-relative globs expanded
/// with `compgen -G` (dotglob honors `include-hidden-files`); each
/// artifact is one uncompressed tar preserving workspace-relative
/// paths, like the action's v4 hierarchy.
fn upload_artifact_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "name",
            "path",
            "if-no-files-found",
            "retention-days",
            "compression-level",
            "archive",
            "include-hidden-files",
            "overwrite",
        ],
        what,
    )?;
    let name = with_one(pairs, "name", what)?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{what}.with.name: required"))?;
    if !artifact_name_ok(name) {
        return Err(format!("{what}.with.name: invalid artifact name `{name}`"));
    }
    let path = with_one(pairs, "path", what)?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{what}.with.path: required"))?;
    let inf = with_one(pairs, "if-no-files-found", what)?.unwrap_or("warn");
    if !matches!(inf, "warn" | "ignore" | "error") {
        return Err(format!(
            "{what}.with.if-no-files-found: expected warn, ignore or error, got `{inf}`"
        ));
    }
    let hidden = with_flag(pairs, "include-hidden-files", what)?.unwrap_or(true);
    let overwrite = with_flag(pairs, "overwrite", what)?.unwrap_or(false);
    let ignored = ["retention-days", "compression-level", "archive"]
        .iter()
        .any(|k| pairs.iter().any(|(pk, _)| pk == k));

    let mut s = String::from("set -e\n");
    s.push_str(&format!("name={}\n", bash_sq(name)));
    s.push_str(&format!("upaths={}\n", bash_sq(path)));
    s.push_str(&format!("inf={}\n", bash_sq(inf)));
    s.push_str(&format!("hidden={}\n", if hidden { "1" } else { "0" }));
    s.push_str(&format!("ovw={}\n", if overwrite { "1" } else { "0" }));
    if ignored {
        s.push_str(
            "echo \"gha: upload-artifact — retention-days/compression-level/archive \
             are no-ops in the per-run artifact store\"\n",
        );
    }
    s.push_str(
        "store=\"$BUPHA_GHA_TMP/artifacts\"\n\
         mkdir -p \"$store\"\n\
         if [ \"$hidden\" = \"1\" ]; then shopt -s dotglob; fi\n\
         mapfile -t _entries < <(printf '%s\\n' \"$upaths\")\n\
         _matches=()\n\
         for _e in \"${_entries[@]}\"; do\n\
         \x20 _e=\"${_e#\"${_e%%[![:space:]]*}\"}\"\n\
         \x20 _e=\"${_e%\"${_e##*[![:space:]]}\"}\"\n\
         \x20 [ -z \"$_e\" ] && continue\n\
         \x20 case \"$_e\" in\n\
         \x20   \"$GITHUB_WORKSPACE\"/*) _e=\"${_e#\"$GITHUB_WORKSPACE\"/}\" ;;\n\
         \x20   /*) echo \"gha: upload-artifact — absolute path '$_e' is unsupported (paths are workspace-relative)\"; exit 1 ;;\n\
         \x20 esac\n\
         \x20 while IFS= read -r _m; do _matches+=(\"$_m\"); done < <(compgen -G \"$_e\")\n\
         done\n\
         if [ \"${#_matches[@]}\" -eq 0 ]; then\n\
         \x20 case \"$inf\" in\n\
         \x20   error) echo \"gha: upload-artifact — no files found\"; exit 1 ;;\n\
         \x20   ignore) echo \"gha: upload-artifact — no files found (ignored)\"; exit 0 ;;\n\
         \x20   *) echo \"::warning::gha: upload-artifact — no files found for the given path(s)\" ;;\n\
         \x20 esac\n\
         else\n\
         \x20 declare -A _seen=()\n\
         \x20 _uniq=()\n\
         \x20 for _m in \"${_matches[@]}\"; do\n\
         \x20   if [ -z \"${_seen[$_m]:-}\" ]; then _seen[$_m]=1; _uniq+=(\"$_m\"); fi\n\
         \x20 done\n\
         \x20 _out=\"$store/$name.tar\"\n\
         \x20 if [ -e \"$_out\" ] && [ \"$ovw\" != \"1\" ]; then\n\
         \x20   echo \"gha: upload-artifact — artifact '$name' already exists; set overwrite: true to replace it\"\n\
         \x20   exit 1\n\
         \x20 fi\n\
         \x20 tar -cf \"$_out\" -C \"$GITHUB_WORKSPACE\" -- \"${_uniq[@]}\"\n\
         \x20 echo \"gha: upload-artifact — '$name' archived ${#_uniq[@]} path(s)\"\n\
         fi\n",
    );
    Ok(s)
}

/// `actions/download-artifact` lowered to bash over the same
/// per-run store. Same-run only: `run-id`/`github-token`/
/// `repository`/`run-attempt` hard-error at plan time. Without
/// `merge-multiple` each artifact extracts into `<path>/<name>/`;
/// `pattern` (a name glob) downloads every match.
fn download_artifact_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    for (k, _) in pairs {
        if matches!(
            k.as_str(),
            "run-id" | "github-token" | "repository" | "run-attempt"
        ) {
            return Err(format!(
                "{what}.with.{k}: cross-run artifact download is unsupported — \
                 the local store only covers the current run"
            ));
        }
    }
    check_with_keys(pairs, &["name", "path", "pattern", "merge-multiple"], what)?;
    let name = with_one(pairs, "name", what)?
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let pat = with_one(pairs, "pattern", what)?
        .map(str::trim)
        .filter(|s| !s.is_empty());
    if name.is_none() && pat.is_none() {
        return Err(format!("{what}.with: `name` or `pattern` is required"));
    }
    if let Some(n) = name {
        if !artifact_name_ok(n) {
            return Err(format!("{what}.with.name: invalid artifact name `{n}`"));
        }
    }
    if let Some(p) = pat {
        if p.chars().any(|c| c.is_control()) {
            return Err(format!(
                "{what}.with.pattern: control characters are unsupported"
            ));
        }
    }
    let dest = with_one(pairs, "path", what)?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(".");
    let merge = with_flag(pairs, "merge-multiple", what)?.unwrap_or(false);

    let mut s = String::from("set -e\n");
    s.push_str(&format!("name={}\n", bash_sq(name.unwrap_or(""))));
    s.push_str(&format!("pat={}\n", bash_sq(pat.unwrap_or(""))));
    s.push_str(&format!("dest={}\n", bash_sq(dest)));
    s.push_str(&format!("merge={}\n", if merge { "1" } else { "0" }));
    s.push_str(
        "store=\"$BUPHA_GHA_TMP/artifacts\"\n\
         mkdir -p \"$dest\"\n\
         shopt -s nullglob\n\
         declare -a _arts=()\n\
         if [ -n \"$name\" ]; then\n\
         \x20 _f=\"$store/$name.tar\"\n\
         \x20 if [ ! -f \"$_f\" ]; then\n\
         \x20   echo \"gha: download-artifact — artifact '$name' was not uploaded in this run\"\n\
         \x20   exit 1\n\
         \x20 fi\n\
         \x20 _arts+=(\"$_f|$name\")\n\
         elif [ -n \"$pat\" ]; then\n\
         \x20 for _f in \"$store\"/*.tar; do\n\
         \x20   [ -f \"$_f\" ] || continue\n\
         \x20   _b=${_f##*/}; _b=${_b%.tar}\n\
         \x20   case \"$_b\" in $pat) _arts+=(\"$_f|$_b\") ;; esac\n\
         \x20 done\n\
         fi\n\
         if [ \"${#_arts[@]}\" -eq 0 ]; then\n\
         \x20 echo \"gha: download-artifact — no artifacts matched\"\n\
         \x20 exit 1\n\
         fi\n\
         if [ \"$merge\" = \"1\" ]; then\n\
         \x20 for _a in \"${_arts[@]}\"; do tar -xf \"${_a%%|*}\" -C \"$dest\"; done\n\
         else\n\
         \x20 for _a in \"${_arts[@]}\"; do\n\
         \x20   _d=\"$dest/${_a#*|}\"\n\
         \x20   mkdir -p \"$_d\"\n\
         \x20   tar -xf \"${_a%%|*}\" -C \"$_d\"\n\
         \x20 done\n\
         fi\n\
         echo \"gha: download-artifact — restored ${#_arts[@]} artifact(s) into '$dest'\"\n",
    );
    Ok(s)
}

/// Kahn topo sort, declaration order breaking ties. Indices in
/// `needs` refer to the input vec; the output re-maps them.
fn topo_order(jobs: Vec<Job>) -> Result<Vec<Job>, String> {
    let n = jobs.len();
    let mut slots: Vec<Option<Job>> = jobs.into_iter().map(Some).collect();
    let mut out_pos = vec![usize::MAX; n];
    let mut emitted = vec![false; n];
    let mut order: Vec<usize> = Vec::with_capacity(n);
    for _ in 0..n {
        let mut progressed = false;
        for i in 0..n {
            if emitted[i] {
                continue;
            }
            let ready = slots[i]
                .as_ref()
                .map(|j| j.needs.iter().all(|&d| emitted[d]))
                .unwrap_or(false);
            if !ready {
                continue;
            }
            emitted[i] = true;
            out_pos[i] = order.len();
            order.push(i);
            progressed = true;
        }
        if order.len() == n {
            break;
        }
        if !progressed {
            return Err("jobs: dependency cycle in `needs`".into());
        }
    }
    let mut out = Vec::with_capacity(n);
    for i in order {
        if let Some(mut j) = slots[i].take() {
            j.needs = j.needs.iter().map(|&d| out_pos[d]).collect();
            j.needs.sort_unstable();
            j.needs.dedup();
            out.push(j);
        }
    }
    Ok(out)
}

// ------------------------------------------------------------------
// ${} expressions
// ------------------------------------------------------------------

/// Replace every `${{ expr }}` in `tpl`. Anything that isn't a
/// plain dotted path is an explicit error.
pub fn interpolate(tpl: &str, eval: &Eval, what: &str) -> Result<String, String> {
    let mut out = String::with_capacity(tpl.len());
    let mut rest = tpl;
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 3..];
        let Some(end) = after.find("}}") else {
            return Err(format!("{what}: unclosed `${{{{`"));
        };
        out.push_str(&resolve(after[..end].trim(), eval, what)?);
        rest = &after[end + 2..];
    }
    out.push_str(rest);
    Ok(out)
}

/// Is `inner` a runtime-deferred ref — `needs.<job>.outputs.<name>`,
/// `steps.<id>.outputs.<name>`, `needs.<job>.result` or
/// `steps.<id>.conclusion`? These resolve while the generated
/// program runs (status arrays + output stores), so the `${{ … }}`
/// survives expand verbatim for the renderer. The output split is
/// at the FIRST `.outputs.`: the middle is the job/step id,
/// everything after is the output name (dots allowed).
fn is_deferred_ref(inner: &str) -> bool {
    if is_output_ref(inner) {
        return true;
    }
    for (prefix, suffix) in [("needs.", ".result"), ("steps.", ".conclusion")] {
        if let Some(rest) = inner.strip_prefix(prefix) {
            if let Some(id) = rest.strip_suffix(suffix) {
                return !id.is_empty();
            }
        }
    }
    false
}

fn is_output_ref(inner: &str) -> bool {
    for prefix in ["needs.", "steps."] {
        if let Some(rest) = inner.strip_prefix(prefix) {
            if let Some((head, name)) = rest.split_once(".outputs.") {
                return !head.is_empty() && !name.is_empty();
            }
        }
    }
    false
}

/// Names that become path components in the renderer's output store
/// (job `outputs:` keys, step `id:`s): `[A-Za-z0-9_.-]`, never `.`
/// or `..`. Expression-derived names are already charset-checked by
/// `resolve`; these YAML-side names need the same guarantee.
fn safe_key(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
}

fn resolve(inner: &str, eval: &Eval, what: &str) -> Result<String, String> {
    let bad = || {
        format!(
            "{what}: unsupported expression `${{{{ {inner} }}}}` — supported: \
             env.*, github.{{sha,ref,ref_name,repository,event_name}}, matrix.*, \
             needs.<job>.outputs.<name>, steps.<id>.outputs.<name>"
        )
    };
    let dotted_ok = !inner.is_empty()
        && inner
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'));
    if !dotted_ok {
        return Err(bad());
    }
    let (root, rest) = inner.split_once('.').unwrap_or((inner, ""));
    match root {
        "env" => {
            if rest.is_empty() {
                return Err(bad());
            }
            let Some(env) = eval.env else {
                return Err(format!("{what}: `env.*` is not available in this position"));
            };
            // Undefined env expands empty, like GitHub.
            Ok(env
                .iter()
                .rev()
                .find(|(k, _)| k == rest)
                .map(|(_, v)| v.clone())
                .unwrap_or_default())
        }
        "github" => match rest {
            "sha" => Ok(eval.ctx.sha.to_string()),
            "ref" => Ok(eval.ctx.git_ref.to_string()),
            "ref_name" => Ok(short_ref(eval.ctx.git_ref).to_string()),
            "repository" => Ok(eval.ctx.repository.to_string()),
            "event_name" => Ok(eval.ctx.event_name.to_string()),
            _ => Err(format!(
                "{what}: unsupported github context `{inner}` — supported: \
                 sha, ref, ref_name, repository, event_name"
            )),
        },
        "matrix" => {
            if rest.is_empty() {
                return Err(bad());
            }
            eval.matrix
                .iter()
                .find(|(k, _)| k == rest)
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("{what}: matrix has no `{rest}` binding"))
        }
        "needs" | "steps" => {
            // Runtime refs can't resolve at expand time — the values
            // are produced while the generated program runs. Keep the
            // `${{ … }}` source verbatim; the renderer turns it into
            // a lookup against the status arrays / output stores.
            // Everything else under these contexts stays unsupported.
            if is_deferred_ref(inner) {
                Ok(format!("${{{{ {inner} }}}}"))
            } else {
                Err(format!(
                    "{what}: unsupported expression `${{{{ {inner} }}}}` — \
                     under `needs`/`steps` only `.outputs.<name>`, \
                     `needs.*.result` and `steps.*.conclusion` refs are \
                     supported (steps.*.outcome and friends are not)"
                ))
            }
        }
        _ => Err(bad()),
    }
}

// ------------------------------------------------------------------
// parsing
// ------------------------------------------------------------------

/// Parse + validate one workflow file. `source` names the file in
/// errors.
pub fn parse_workflow(source: &str, text: &str) -> Result<Workflow, String> {
    let v: Value =
        serde_norway::from_str(text).map_err(|e| format!("{source}: YAML parse error: {e}"))?;
    let top = as_map(&v, source)?;
    check_keys(
        top,
        &[
            "name",
            "run-name",
            "on",
            "env",
            "defaults",
            "jobs",
            "permissions",
        ],
        &[],
        source,
    )?;
    let name = get(top, "name")
        .map(|n| scalar(n, &format!("{source}.name")))
        .transpose()?
        .unwrap_or_else(|| {
            source
                .rsplit('/')
                .next()
                .unwrap_or(source)
                .trim_end_matches(".yml")
                .trim_end_matches(".yaml")
                .to_string()
        });
    let env = match get(top, "env") {
        Some(Value::Mapping(m)) => env_map(m, &format!("{source}.env"))?,
        Some(v) => return Err(format!("{source}.env: expected a mapping, got {}", kind(v))),
        None => Vec::new(),
    };
    let defaults = parse_defaults(get(top, "defaults"), &format!("{source}.defaults"))?;
    let filters = parse_on(get(top, "on"), &format!("{source}.on"))?;
    let jobs_v = get(top, "jobs").ok_or_else(|| format!("{source}: missing `jobs`"))?;
    let jobs_m = as_map(jobs_v, &format!("{source}.jobs"))?;
    if jobs_m.is_empty() {
        return Err(format!("{source}: `jobs` is empty"));
    }
    let mut jobs = Vec::with_capacity(jobs_m.len());
    for (k, jv) in jobs_m {
        let id = k
            .as_str()
            .ok_or_else(|| format!("{source}.jobs: job ids must be strings"))?
            .to_string();
        jobs.push(parse_job(
            as_map(jv, &format!("{source}.jobs.{id}"))?,
            &id,
            source,
        )?);
    }
    let ids: HashSet<&str> = jobs.iter().map(|j| j.id.as_str()).collect();
    for j in &jobs {
        for n in &j.needs {
            if !ids.contains(n.as_str()) {
                return Err(format!(
                    "{source}.jobs.{}: `needs` references unknown job `{n}`",
                    j.id
                ));
            }
        }
    }
    Ok(Workflow {
        name,
        env,
        defaults,
        filters,
        jobs,
    })
}

fn parse_defaults(v: Option<&Value>, what: &str) -> Result<Defaults, String> {
    let Some(v) = v else {
        return Ok(Defaults::default());
    };
    let m = as_map(v, what)?;
    check_keys(m, &["run"], &[], what)?;
    let Some(rv) = get(m, "run") else {
        return Ok(Defaults::default());
    };
    let rm = as_map(rv, &format!("{what}.run"))?;
    check_keys(
        rm,
        &["working-directory", "shell"],
        &[],
        &format!("{what}.run"),
    )?;
    Ok(Defaults {
        working_directory: get(rm, "working-directory")
            .map(|s| scalar(s, &format!("{what}.run.working-directory")))
            .transpose()?,
        shell: get(rm, "shell")
            .map(|s| scalar(s, &format!("{what}.run.shell")))
            .transpose()?,
    })
}

fn parse_job(m: &serde_norway::Mapping, id: &str, source: &str) -> Result<RawJob, String> {
    let what = format!("{source}.jobs.{id}");
    check_keys(
        m,
        &[
            "name",
            "if",
            "needs",
            "runs-on",
            "env",
            "defaults",
            "steps",
            "strategy",
            "permissions",
            "outputs",
            "timeout-minutes",
        ],
        // The v1 boundary, loudly: containers, services,
        // environment, concurrency, reusable-workflow calls.
        &[
            "services",
            "container",
            "environment",
            "concurrency",
            "continue-on-error",
            "uses",
            "secrets",
        ],
        &what,
    )?;
    let name = get(m, "name")
        .map(|n| scalar(n, &format!("{what}.name")))
        .transpose()?;
    let cond = get(m, "if")
        .map(|v| scalar(v, &format!("{what}.if")))
        .transpose()?;
    let timeout_seconds = get(m, "timeout-minutes")
        .map(|v| timeout_minutes(v, &format!("{what}.timeout-minutes")))
        .transpose()?;
    let needs = match get(m, "needs") {
        Some(v) => string_list(v, &format!("{what}.needs"))?,
        None => Vec::new(),
    };
    let runs_on = match get(m, "runs-on") {
        Some(v) => string_list(v, &format!("{what}.runs-on"))?,
        None => Vec::new(),
    };
    let env = match get(m, "env") {
        Some(Value::Mapping(em)) => env_map(em, &format!("{what}.env"))?,
        Some(v) => return Err(format!("{what}.env: expected a mapping, got {}", kind(v))),
        None => Vec::new(),
    };
    let defaults = parse_defaults(get(m, "defaults"), &format!("{what}.defaults"))?;
    let strategy = parse_strategy(get(m, "strategy"), &what)?;
    let outputs = match get(m, "outputs") {
        Some(Value::Mapping(om)) => {
            let mut out = Vec::with_capacity(om.len());
            for (k, v) in om {
                let key = k
                    .as_str()
                    .ok_or_else(|| format!("{what}.outputs: names must be strings"))?;
                if !safe_key(key) {
                    return Err(format!(
                        "{what}.outputs: invalid output name `{key}` — \
                         `[A-Za-z0-9_.-]` only"
                    ));
                }
                out.push((
                    key.to_string(),
                    scalar(v, &format!("{what}.outputs.{key}"))?,
                ));
            }
            out
        }
        Some(v) => {
            return Err(format!(
                "{what}.outputs: expected a mapping, got {}",
                kind(v)
            ))
        }
        None => Vec::new(),
    };
    let steps_v = get(m, "steps").ok_or_else(|| format!("{what}: missing `steps`"))?;
    let Value::Sequence(seq) = steps_v else {
        return Err(format!("{what}.steps: expected a sequence"));
    };
    if seq.is_empty() {
        return Err(format!("{what}.steps: empty"));
    }
    let mut steps = Vec::with_capacity(seq.len());
    let mut step_ids: HashSet<String> = HashSet::new();
    for (si, sv) in seq.iter().enumerate() {
        let sw = format!("{what}.steps[{si}]");
        let sm = as_map(sv, &sw)?;
        // `id:` is the steps.<id>.outputs.* handle — it becomes a
        // path component in the renderer's output store, so it gets
        // the same charset rule as job output names. Duplicate ids
        // are ambiguous, like on GitHub.
        let id = get(sm, "id")
            .map(|s| scalar(s, &format!("{sw}.id")))
            .transpose()?;
        if let Some(id) = &id {
            if !safe_key(id) {
                return Err(format!(
                    "{sw}.id: invalid step id `{id}` — `[A-Za-z0-9_.-]` only"
                ));
            }
            if !step_ids.insert(id.clone()) {
                return Err(format!("{sw}.id: duplicate step id `{id}`"));
            }
        }
        check_keys(
            sm,
            &[
                "id",
                "name",
                "if",
                "run",
                "uses",
                "with",
                "working-directory",
                "env",
                "shell",
                "timeout-minutes",
            ],
            &["continue-on-error"],
            &sw,
        )?;
        let cond = get(sm, "if")
            .map(|v| scalar(v, &format!("{sw}.if")))
            .transpose()?;
        let timeout_seconds = get(sm, "timeout-minutes")
            .map(|v| timeout_minutes(v, &format!("{sw}.timeout-minutes")))
            .transpose()?;
        let run = get(sm, "run");
        let uses = get(sm, "uses");
        match (run, uses) {
            (Some(_), Some(_)) => {
                return Err(format!("{sw}: step cannot have both `run` and `uses`"))
            }
            (None, None) => return Err(format!("{sw}: step needs `run` or `uses`")),
            (Some(rv), None) => {
                if get(sm, "with").is_some() {
                    return Err(format!("{sw}: `with` requires `uses`"));
                }
                let Value::String(script) = rv else {
                    return Err(format!("{sw}.run: expected a string"));
                };
                let env = match get(sm, "env") {
                    Some(Value::Mapping(em)) => env_map(em, &format!("{sw}.env"))?,
                    Some(v) => {
                        return Err(format!("{sw}.env: expected a mapping, got {}", kind(v)))
                    }
                    None => Vec::new(),
                };
                steps.push(RawStep::Run {
                    id,
                    cond,
                    timeout_seconds,
                    shell: get(sm, "shell")
                        .map(|s| scalar(s, &format!("{sw}.shell")))
                        .transpose()?,
                    working_directory: get(sm, "working-directory")
                        .map(|s| scalar(s, &format!("{sw}.working-directory")))
                        .transpose()?,
                    env,
                    script: script.clone(),
                });
            }
            (None, Some(uv)) => {
                let uses = scalar(uv, &format!("{sw}.uses"))?;
                match parse_uses(&uses, sm, &sw)? {
                    UsesKind::Checkout => steps.push(RawStep::Checkout { id, cond }),
                    UsesKind::Script(kind, with) => steps.push(RawStep::Script {
                        kind,
                        id,
                        cond,
                        timeout_seconds,
                        with,
                    }),
                }
            }
        }
    }
    Ok(RawJob {
        id: id.to_string(),
        name,
        cond,
        needs,
        runs_on,
        env,
        defaults,
        matrix: strategy.matrix,
        fail_fast: strategy.fail_fast,
        max_parallel: strategy.max_parallel,
        timeout_seconds,
        outputs,
        steps,
    })
}

/// `timeout-minutes:` → whole seconds. GitHub accepts floats
/// (0.5 = 30s); non-positive or non-numeric values are errors.
fn timeout_minutes(v: &Value, what: &str) -> Result<u64, String> {
    let s = scalar(v, what)?;
    let mins: f64 = s
        .trim()
        .parse()
        .map_err(|_| format!("{what}: expected a number of minutes, got `{s}`"))?;
    if !mins.is_finite() || mins <= 0.0 {
        return Err(format!("{what}: must be a positive number of minutes"));
    }
    Ok((mins * 60.0).ceil() as u64)
}

/// `uses:` targets that lower to a synthesized bash step.
#[derive(Clone, Copy)]
enum UsesScript {
    /// `dtolnay/rust-toolchain` → rustup install/default.
    RustToolchain,
    /// `actions/upload-artifact` → per-run tar store.
    UploadArtifact,
    /// `actions/download-artifact` → restore from the per-run store.
    DownloadArtifact,
}

enum UsesKind {
    Checkout,
    /// Script-lowered `uses:` + `with:` pairs normalized to value
    /// lists.
    Script(UsesScript, Vec<(String, Vec<String>)>),
}

/// `uses:` — `actions/checkout` is a verification no-op (the
/// worktree is already at the pushed sha); `dtolnay/rust-toolchain`,
/// `actions/upload-artifact` and `actions/download-artifact` lower
/// to synthesized bash steps. Everything else, `actions/setup-*`
/// included, is an explicit error.
fn parse_uses(uses: &str, sm: &serde_norway::Mapping, what: &str) -> Result<UsesKind, String> {
    let unsupported = || {
        format!(
            "{what}: unsupported `uses: {uses}` — v1 supports \
             `actions/checkout`, `dtolnay/rust-toolchain`, \
             `actions/upload-artifact` and `actions/download-artifact` \
             (`actions/setup-*` is a later milestone)"
        )
    };
    let body = uses.split('@').next().unwrap_or(uses);
    if body.starts_with("docker://") || body.starts_with("./") || body.starts_with('/') {
        return Err(unsupported());
    }
    let script = if body.eq_ignore_ascii_case("actions/checkout") {
        None
    } else if body.eq_ignore_ascii_case("dtolnay/rust-toolchain") {
        Some(UsesScript::RustToolchain)
    } else if body.eq_ignore_ascii_case("actions/upload-artifact") {
        Some(UsesScript::UploadArtifact)
    } else if body.eq_ignore_ascii_case("actions/download-artifact") {
        Some(UsesScript::DownloadArtifact)
    } else {
        return Err(unsupported());
    };
    let with = match get(sm, "with") {
        Some(Value::Mapping(w)) => Some(w),
        Some(_) => return Err(format!("{what}.with: expected a mapping")),
        None => None,
    };
    let Some(kind) = script else {
        // actions/checkout — `with.path` errors (the worktree root
        // is fixed); other inputs (ref, fetch-depth, token, …)
        // don't change local semantics: the sha is checked out.
        if let Some(w) = with {
            if get(w, "path").is_some() {
                return Err(format!(
                    "{what}: `with.path` on actions/checkout is unsupported — \
                     the worktree root is fixed"
                ));
            }
        }
        return Ok(UsesKind::Checkout);
    };
    // Script-lowered uses: — normalize `with:` values to lists
    // (scalar → one entry, sequence of scalars → many).
    let mut out = Vec::new();
    if let Some(w) = with {
        out.reserve(w.len());
        for (k, v) in w {
            let key = k
                .as_str()
                .ok_or_else(|| format!("{what}.with: input names must be strings"))?;
            let vals = match v {
                Value::Sequence(seq) => seq
                    .iter()
                    .map(|i| scalar(i, &format!("{what}.with.{key}")))
                    .collect::<Result<Vec<_>, _>>()?,
                _ => vec![scalar(v, &format!("{what}.with.{key}"))?],
            };
            out.push((key.to_string(), vals));
        }
    }
    Ok(UsesKind::Script(kind, out))
}

struct Strategy {
    matrix: Vec<(String, Vec<String>)>,
    /// GHA default: fail-fast is on when a matrix exists.
    fail_fast: bool,
    max_parallel: Option<u32>,
}

fn parse_strategy(v: Option<&Value>, what: &str) -> Result<Strategy, String> {
    let Some(v) = v else {
        return Ok(Strategy {
            matrix: Vec::new(),
            fail_fast: false,
            max_parallel: None,
        });
    };
    let m = as_map(v, &format!("{what}.strategy"))?;
    check_keys(
        m,
        &["matrix", "fail-fast", "max-parallel"],
        &[],
        &format!("{what}.strategy"),
    )?;
    let mut fail_fast = None;
    let mut max_parallel = None;
    if let Some(fv) = get(m, "fail-fast") {
        fail_fast = Some(match fv {
            Value::Bool(b) => *b,
            Value::String(s) => s == "true",
            _ => {
                return Err(format!(
                    "{what}.strategy.fail-fast: expected a bool, got {}",
                    kind(fv)
                ))
            }
        });
    }
    if let Some(pv) = get(m, "max-parallel") {
        let s = scalar(pv, &format!("{what}.strategy.max-parallel"))?;
        let n: u32 = s.parse().map_err(|_| {
            format!("{what}.strategy.max-parallel: expected a positive integer, got `{s}`")
        })?;
        if n == 0 {
            return Err(format!("{what}.strategy.max-parallel: must be >= 1"));
        }
        max_parallel = Some(n);
    }
    let Some(mv) = get(m, "matrix") else {
        return Ok(Strategy {
            matrix: Vec::new(),
            fail_fast: false,
            max_parallel,
        });
    };
    let mm = as_map(mv, &format!("{what}.strategy.matrix"))?;
    let mut matrix = Vec::with_capacity(mm.len());
    for (k, vv) in mm {
        let key = k
            .as_str()
            .ok_or_else(|| format!("{what}.strategy.matrix: keys must be strings"))?;
        let Value::Sequence(seq) = vv else {
            return Err(format!(
                "{what}.strategy.matrix.{key}: expected a list — \
                 `include`/`exclude` are not supported"
            ));
        };
        if seq.is_empty() {
            return Err(format!("{what}.strategy.matrix.{key}: empty list"));
        }
        let vals = seq
            .iter()
            .map(|i| scalar(i, &format!("{what}.strategy.matrix.{key}")))
            .collect::<Result<Vec<_>, _>>()?;
        matrix.push((key.to_string(), vals));
    }
    // fail-fast only has meaning with a matrix; GHA defaults it on.
    Ok(Strategy {
        fail_fast: fail_fast.unwrap_or(true),
        matrix,
        max_parallel,
    })
}

/// Cartesian product of matrix axes; no matrix → one empty combo.
fn matrix_combos(
    axes: &[(String, Vec<String>)],
    what: &str,
) -> Result<Vec<Vec<(String, String)>>, String> {
    let mut combos: Vec<Vec<(String, String)>> = vec![Vec::new()];
    for (name, vals) in axes {
        let mut next = Vec::with_capacity(combos.len() * vals.len());
        for c in &combos {
            for v in vals {
                let mut c = c.clone();
                c.push((name.clone(), v.clone()));
                next.push(c);
            }
        }
        combos = next;
    }
    if combos.is_empty() {
        return Err(format!("{what}: matrix produced zero combinations"));
    }
    Ok(combos)
}

// ------------------------------------------------------------------
// `if:` expression parsing — recursive descent over a small GHA
// expression subset, then context folding via `Eval`
// ------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq)]
enum Tok {
    LParen,
    RParen,
    Not,
    And,
    Or,
    Eq,
    Ne,
    Comma,
    /// 'single' or "double" quoted literal contents.
    Str(String),
    /// Bare word — dotted path, call name, number, true/false/null.
    Word(String),
}

fn lex_cond(src: &str, what: &str) -> Result<Vec<Tok>, String> {
    let b = src.as_bytes();
    let mut i = 0;
    let mut out = Vec::new();
    while i < b.len() {
        let c = b[i] as char;
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        match c {
            '(' => {
                out.push(Tok::LParen);
                i += 1;
            }
            ')' => {
                out.push(Tok::RParen);
                i += 1;
            }
            '!' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Ne);
                    i += 2;
                } else {
                    out.push(Tok::Not);
                    i += 1;
                }
            }
            '&' => {
                if b.get(i + 1) == Some(&b'&') {
                    out.push(Tok::And);
                    i += 2;
                } else {
                    return Err(format!("{what}: bare `&` in `if` expression"));
                }
            }
            '|' => {
                if b.get(i + 1) == Some(&b'|') {
                    out.push(Tok::Or);
                    i += 2;
                } else {
                    return Err(format!("{what}: bare `|` in `if` expression"));
                }
            }
            '=' => {
                if b.get(i + 1) == Some(&b'=') {
                    out.push(Tok::Eq);
                    i += 2;
                } else {
                    return Err(format!("{what}: bare `=` in `if` expression"));
                }
            }
            ',' => {
                out.push(Tok::Comma);
                i += 1;
            }
            '\'' | '"' => {
                let quote = b[i];
                i += 1;
                let start = i;
                while i < b.len() && b[i] != quote {
                    i += 1;
                }
                if i >= b.len() {
                    return Err(format!("{what}: unterminated string in `if`"));
                }
                out.push(Tok::Str(src[start..i].to_string()));
                i += 1;
            }
            _ => {
                let start = i;
                while i < b.len() {
                    let c = b[i] as char;
                    if c.is_whitespace()
                        || matches!(c, '(' | ')' | '!' | '&' | '|' | '=' | ',' | '\'' | '"')
                    {
                        break;
                    }
                    i += 1;
                }
                if start == i {
                    return Err(format!("{what}: unexpected character `{c}` in `if`"));
                }
                out.push(Tok::Word(src[start..i].to_string()));
            }
        }
    }
    Ok(out)
}

/// Pre-fold AST — paths and calls still raw; `fold` resolves
/// env/github/matrix and builds [`Cond`].
enum PExpr {
    Str(String),
    Path(String),
    Call {
        name: String,
        args: Vec<PExpr>,
    },
    Bin {
        op: BinOp,
        l: Box<PExpr>,
        r: Box<PExpr>,
    },
    Not(Box<PExpr>),
}

struct CondParser<'a> {
    toks: &'a [Tok],
    pos: usize,
    what: &'a str,
}

impl CondParser<'_> {
    fn peek(&self) -> Option<&Tok> {
        self.toks.get(self.pos)
    }

    fn next(&mut self) -> Option<Tok> {
        let t = self.toks.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    fn err(&self, msg: &str) -> String {
        format!("{}: {msg} in `if` expression", self.what)
    }

    /// or := and ('||' and)*
    fn parse_or(&mut self) -> Result<PExpr, String> {
        let mut l = self.parse_and()?;
        while matches!(self.peek(), Some(Tok::Or)) {
            self.next();
            let r = self.parse_and()?;
            l = PExpr::Bin {
                op: BinOp::Or,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    /// and := eq ('&&' eq)*
    fn parse_and(&mut self) -> Result<PExpr, String> {
        let mut l = self.parse_eq()?;
        while matches!(self.peek(), Some(Tok::And)) {
            self.next();
            let r = self.parse_eq()?;
            l = PExpr::Bin {
                op: BinOp::And,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    /// eq := unary (('=='|'!=') unary)*
    fn parse_eq(&mut self) -> Result<PExpr, String> {
        let mut l = self.parse_unary()?;
        loop {
            let op = match self.peek() {
                Some(Tok::Eq) => BinOp::Eq,
                Some(Tok::Ne) => BinOp::Ne,
                _ => break,
            };
            self.next();
            let r = self.parse_unary()?;
            l = PExpr::Bin {
                op,
                l: Box::new(l),
                r: Box::new(r),
            };
        }
        Ok(l)
    }

    /// unary := '!' unary | primary
    fn parse_unary(&mut self) -> Result<PExpr, String> {
        if matches!(self.peek(), Some(Tok::Not)) {
            self.next();
            return Ok(PExpr::Not(Box::new(self.parse_unary()?)));
        }
        self.parse_primary()
    }

    fn parse_primary(&mut self) -> Result<PExpr, String> {
        match self.next() {
            Some(Tok::LParen) => {
                let e = self.parse_or()?;
                match self.next() {
                    Some(Tok::RParen) => Ok(e),
                    _ => Err(self.err("missing `)`")),
                }
            }
            Some(Tok::Str(s)) => Ok(PExpr::Str(s)),
            Some(Tok::Word(w)) => {
                if matches!(self.peek(), Some(Tok::LParen)) {
                    self.parse_call(w)
                } else {
                    Ok(PExpr::Path(w))
                }
            }
            Some(t) => Err(self.err(&format!("unexpected token {t:?}"))),
            None => Err(self.err("unexpected end of expression")),
        }
    }

    /// Word already consumed, `(` is next — a function call.
    fn parse_call(&mut self, word: String) -> Result<PExpr, String> {
        let name = word.to_lowercase();
        self.next(); // consume '('
        let mut args = Vec::new();
        if !matches!(self.peek(), Some(Tok::RParen)) {
            loop {
                args.push(self.parse_or()?);
                match self.next() {
                    Some(Tok::Comma) => {}
                    Some(Tok::RParen) => break,
                    _ => return Err(self.err("expected `,` or `)` in call")),
                }
            }
        } else {
            self.next();
        }
        Ok(PExpr::Call { name, args })
    }
}

/// Parse + fold one `if:` value. The raw text may be a bare
/// expression or a single `${{ … }}` wrapper (mixed text errors).
/// env/github/matrix fold via `eval`; needs/steps become `DynRef`s;
/// an `if` without a status-check function gets `success()` ANDed
/// on, like GitHub.
fn parse_cond(raw: &str, eval: &Eval, what: &str) -> Result<Cond, String> {
    let text = raw.trim();
    let inner = if text.starts_with("${{") {
        if !text.ends_with("}}") || text.matches("${{").count() != 1 {
            return Err(format!(
                "{what}: `if` must be a single `${{{{ … }}}}` expression or a bare expression"
            ));
        }
        text[3..text.len() - 2].trim()
    } else if text.contains("${{") {
        return Err(format!(
            "{what}: `${{{{` in `if` must wrap the whole expression"
        ));
    } else {
        text
    };
    if inner.is_empty() {
        return Err(format!("{what}: empty `if` expression"));
    }
    let toks = lex_cond(inner, what)?;
    let mut p = CondParser {
        toks: &toks,
        pos: 0,
        what,
    };
    let expr = p.parse_or()?;
    if p.pos != toks.len() {
        return Err(format!("{what}: trailing tokens in `if` expression"));
    }
    let mut cond = fold_cond(&expr, eval, what)?;
    if !cond.has_status_fn() {
        cond = Cond::Bin {
            op: BinOp::And,
            l: Box::new(Cond::Call {
                name: "success".into(),
                args: Vec::new(),
            }),
            r: Box::new(cond),
        };
    }
    Ok(cond)
}

fn fold_cond(e: &PExpr, eval: &Eval, what: &str) -> Result<Cond, String> {
    Ok(match e {
        PExpr::Str(s) => Cond::Lit(s.clone()),
        PExpr::Path(w) => fold_path(w, eval, what)?,
        PExpr::Not(inner) => Cond::Not(Box::new(fold_cond(inner, eval, what)?)),
        PExpr::Bin { op, l, r } => Cond::Bin {
            op: *op,
            l: Box::new(fold_cond(l, eval, what)?),
            r: Box::new(fold_cond(r, eval, what)?),
        },
        PExpr::Call { name, args } => {
            let arity = match name.as_str() {
                "success" | "failure" | "cancelled" | "always" => 0,
                "contains" | "startswith" | "endswith" => 2,
                _ => {
                    return Err(format!(
                        "{what}: unsupported function `{name}()` in `if` — \
                         supported: success, failure, cancelled, always, \
                         contains, startsWith, endsWith"
                    ))
                }
            };
            if args.len() != arity {
                return Err(format!(
                    "{what}: `{name}()` expects {arity} argument(s), got {}",
                    args.len()
                ));
            }
            Cond::Call {
                name: name.clone(),
                args: args
                    .iter()
                    .map(|a| fold_cond(a, eval, what))
                    .collect::<Result<_, _>>()?,
            }
        }
    })
}

/// Fold a bare word: literals (numbers, true/false/null), env/
/// github/matrix paths → `Cond::Lit`, needs/steps refs → `DynRef`.
fn fold_path(w: &str, eval: &Eval, what: &str) -> Result<Cond, String> {
    let bad = || {
        format!(
            "{what}: unsupported expression `{w}` in `if` — supported: \
             literals, env.*, github.{{sha,ref,ref_name,repository,event_name}}, \
             matrix.*, needs.<job>.{{result,outputs.<name>}}, \
             steps.<id>.{{conclusion,outputs.<name>}}"
        )
    };
    // Literals first — `true`/`false`/`null` and bare numbers.
    if w == "true" {
        return Ok(Cond::Lit("true".into()));
    }
    if w == "false" {
        return Ok(Cond::Lit("false".into()));
    }
    if w == "null" {
        return Ok(Cond::Lit(String::new()));
    }
    if !w.is_empty()
        && w.chars()
            .all(|c| c.is_ascii_digit() || c == '-' || c == '+' || c == '.')
        && w.chars().any(|c| c.is_ascii_digit())
    {
        return Ok(Cond::Lit(w.to_string()));
    }
    let (root, rest) = w.split_once('.').unwrap_or((w, ""));
    match root {
        "env" => {
            if rest.is_empty() {
                return Err(bad());
            }
            let Some(env) = eval.env else {
                return Err(format!("{what}: `env.*` is not available in this position"));
            };
            Ok(Cond::Lit(
                env.iter()
                    .rev()
                    .find(|(k, _)| k == rest)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
            ))
        }
        "github" => {
            let v = match rest {
                "sha" => eval.ctx.sha,
                "ref" => eval.ctx.git_ref,
                "ref_name" => short_ref(eval.ctx.git_ref),
                "repository" => eval.ctx.repository,
                "event_name" => eval.ctx.event_name,
                _ => return Err(bad()),
            };
            Ok(Cond::Lit(v.to_string()))
        }
        "matrix" => {
            if rest.is_empty() {
                return Err(bad());
            }
            eval.matrix
                .iter()
                .find(|(k, _)| k == rest)
                .map(|(_, v)| Cond::Lit(v.clone()))
                .ok_or_else(|| format!("{what}: matrix has no `{rest}` binding in `if`"))
        }
        "needs" => {
            // `.outputs.` wins over `.result` — same precedence as
            // `is_deferred_ref` (an output name may itself end in
            // `.result`).
            if let Some((job, name)) = rest.split_once(".outputs.") {
                if !job.is_empty() && !name.is_empty() {
                    return Ok(Cond::Dyn(DynRef::NeedOutput {
                        job: job.to_string(),
                        name: name.to_string(),
                    }));
                }
                return Err(bad());
            }
            if let Some(job) = rest.strip_suffix(".result") {
                if job.is_empty() {
                    return Err(bad());
                }
                return Ok(Cond::Dyn(DynRef::NeedResult {
                    job: job.to_string(),
                }));
            }
            Err(bad())
        }
        "steps" => {
            if let Some((id, name)) = rest.split_once(".outputs.") {
                if !id.is_empty() && !name.is_empty() {
                    return Ok(Cond::Dyn(DynRef::StepOutput {
                        step: id.to_string(),
                        name: name.to_string(),
                    }));
                }
                return Err(bad());
            }
            if let Some(id) = rest.strip_suffix(".conclusion") {
                if id.is_empty() {
                    return Err(bad());
                }
                return Ok(Cond::Dyn(DynRef::StepConclusion {
                    step: id.to_string(),
                }));
            }
            Err(bad())
        }
        _ => Err(bad()),
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn eval<'a>(matrix: &'a [(String, String)], env: Option<&'a [(String, String)]>) -> Eval<'a> {
        Eval {
            ctx: GhaContext {
                sha: "abc123",
                git_ref: "refs/heads/main",
                repository: "o/r",
                event_name: "push",
            },
            matrix,
            env,
        }
    }

    /// Output refs defer verbatim — the renderer resolves them once
    /// the producing steps have run.
    #[test]
    fn output_refs_defer_to_runtime() {
        let e = eval(&[], None);
        assert_eq!(
            interpolate("pre-${{ needs.build.outputs.sha }}-post", &e, "t").unwrap(),
            "pre-${{ needs.build.outputs.sha }}-post"
        );
        assert_eq!(
            interpolate("${{ steps.gen.outputs.value }}", &e, "t").unwrap(),
            "${{ steps.gen.outputs.value }}"
        );
        // Mixed with resolvable contexts in one template.
        assert_eq!(
            interpolate("${{ github.sha }}:${{ steps.s.outputs.x }}", &e, "t").unwrap(),
            "abc123:${{ steps.s.outputs.x }}"
        );
    }

    /// `needs.*.result` / `steps.*.conclusion` defer like output
    /// refs — the renderer resolves them from runtime state; every
    /// other needs./steps. path still errors loudly.
    #[test]
    fn status_and_output_refs_defer_to_runtime() {
        let e = eval(&[], None);
        for ok in [
            "needs.build.result",
            "steps.s1.conclusion",
            "needs.build.outputs.sha",
            "steps.s1.outputs.value",
            "needs.a.outputs.b.result", // `.outputs.` wins over `.result`
        ] {
            assert_eq!(
                interpolate(&format!("${{{{ {ok} }}}}"), &e, "t").unwrap(),
                format!("${{{{ {ok} }}}}"),
                "{ok} should defer"
            );
        }
        for bad in [
            "needs.build.outputs",
            "needs..outputs.x",
            "steps.s1.outputs",
            "steps..outputs.x",
            "steps.s1.outcome",
            "needs.build.other",
            "steps.s1.outputs.",
        ] {
            assert!(
                interpolate(&format!("${{{{ {bad} }}}}"), &e, "t").is_err(),
                "{bad} should error"
            );
        }
    }

    /// Job `outputs:` parses, interpolates env/github now, and keeps
    /// step refs deferred.
    #[test]
    fn job_outputs_parse_and_expand() {
        let text = r#"
on: push
jobs:
  produce:
    outputs:
      sha_out: ${{ steps.gen.outputs.value }}
      repo: ${{ github.repository }}
    steps:
      - id: gen
        run: echo "value=x" >> "$GITHUB_OUTPUT"
  consume:
    needs: produce
    steps:
      - run: echo "${{ needs.produce.outputs.sha_out }}"
"#;
        let wf = parse_workflow("ci.yml", text).unwrap();
        let jobs = wf
            .expand(GhaContext {
                sha: "s",
                git_ref: "refs/heads/main",
                repository: "o/r",
                event_name: "push",
            })
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert_eq!(jobs[0].id, "produce");
        assert_eq!(
            jobs[0].outputs,
            vec![
                (
                    "sha_out".to_string(),
                    "${{ steps.gen.outputs.value }}".to_string()
                ),
                ("repo".to_string(), "o/r".to_string()),
            ]
        );
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert_eq!(r.id.as_deref(), Some("gen"));
        assert_eq!(
            jobs[1].needs,
            vec![0],
            "consume needs produce (expanded index 0)"
        );
        let Step::Run(c) = &jobs[1].steps[0] else {
            panic!("expected run step");
        };
        assert_eq!(c.script, "echo \"${{ needs.produce.outputs.sha_out }}\"");
    }

    /// Duplicate step ids and unsafe output names are parse errors.
    #[test]
    fn outputs_and_step_ids_are_validated() {
        let dup = r#"
on: push
jobs:
  t:
    steps:
      - id: a
        run: echo 1
      - id: a
        run: echo 2
"#;
        assert!(parse_workflow("ci.yml", dup).is_err());

        let bad_out = r#"
on: push
jobs:
  t:
    outputs:
      "bad/name": ${{ steps.a.outputs.x }}
    steps:
      - id: a
        run: echo x >> "$GITHUB_OUTPUT"
"#;
        assert!(parse_workflow("ci.yml", bad_out).is_err());
    }

    // --- `if:` condition parser -------------------------------------

    fn cond(raw: &str) -> Result<Cond, String> {
        parse_cond(raw, &eval(&[], None), "t.if")
    }

    #[test]
    fn cond_literals_and_paths_fold() {
        // env/github/matrix fold to literals; needs/steps stay dyn.
        assert!(matches!(cond("true").unwrap(), Cond::Bin { .. })); // implicit success() &&
        let matrix = [("os".to_string(), "linux".to_string())];
        let envv = [("FLAG".to_string(), "yes".to_string())];
        let e = eval(&matrix, Some(&envv));
        let c = parse_cond("env.FLAG == 'yes' && matrix.os == 'linux'", &e, "t.if").unwrap();
        let s = format!("{c:?}");
        assert!(
            s.contains("Lit(\"yes\")") && s.contains("Lit(\"linux\")"),
            "{s}"
        );
        let c = cond("github.ref_name == 'main'").unwrap();
        assert!(format!("{c:?}").contains("Lit(\"main\")"));
        let c = cond("needs.build.result == 'success'").unwrap();
        assert!(format!("{c:?}").contains("NeedResult"));
        let c = cond("steps.gen.conclusion == 'skipped'").unwrap();
        assert!(format!("{c:?}").contains("StepConclusion"));
        let c = cond("needs.a.outputs.sha != ''").unwrap();
        assert!(format!("{c:?}").contains("NeedOutput"));
    }

    #[test]
    fn cond_status_functions_and_operators() {
        for raw in [
            "success()",
            "failure() || cancelled()",
            "always() && !cancelled()",
            "!(github.ref == 'x')",
            "contains(github.ref_name, 'main')",
            "startsWith(github.ref_name, 'ma')",
            "endsWith(steps.s.outputs.v, '.txt')",
            "${{ github.event_name == 'push' }}",
        ] {
            assert!(cond(raw).is_ok(), "{raw} should parse");
        }
    }

    #[test]
    fn cond_implicit_success_wrap() {
        // An `if:` without a status function gets success() &&-ed on;
        // one that names a status function doesn't.
        let plain = cond("github.ref_name == 'main'").unwrap();
        assert!(plain.has_status_fn());
        let s = format!("{plain:?}");
        assert!(s.starts_with("Bin { op: And"), "{s}");
        let with_fn = cond("failure()").unwrap();
        assert!(matches!(with_fn, Cond::Call { .. }));
    }

    #[test]
    fn cond_errors_are_loud() {
        for bad in [
            "",
            "${{ a }} ${{ b }}",
            "hashFiles('**')",
            "github.actor == 'x'",
            "needs.a.outputs",
            "contains('x')",
            "success(1)",
            "github.ref_name ==",
            "env.FLAG == 'x' trailing",
        ] {
            assert!(cond(bad).is_err(), "{bad} should error");
        }
    }

    #[test]
    fn timeout_minutes_parse() {
        assert_eq!(
            timeout_minutes(&serde_norway::Value::from(5), "t").unwrap(),
            300
        );
        assert_eq!(
            timeout_minutes(&serde_norway::Value::from(0.5), "t").unwrap(),
            30
        );
        assert!(timeout_minutes(&serde_norway::Value::from(0), "t").is_err());
        assert!(timeout_minutes(&serde_norway::Value::from("x"), "t").is_err());
    }

    #[test]
    fn rust_toolchain_lowers_to_rustup_script() {
        let text = r#"
on: push
jobs:
  t:
    steps:
      - uses: dtolnay/rust-toolchain@stable
        with:
          toolchain: 1.85.0
          components: clippy,rustfmt
"#;
        let wf = parse_workflow("ci.yml", text).unwrap();
        let jobs = wf
            .expand(GhaContext {
                sha: "s",
                git_ref: "refs/heads/main",
                repository: "o/r",
                event_name: "push",
            })
            .unwrap();
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert!(r.script.contains("rustup"), "{}", r.script);
        assert!(r.script.contains("1.85.0"), "{}", r.script);
        assert!(r.script.contains("clippy"), "{}", r.script);
    }

    #[test]
    fn strategy_fail_fast_and_max_parallel_parse() {
        let text = r#"
on: push
jobs:
  t:
    strategy:
      fail-fast: false
      max-parallel: 2
      matrix:
        os: [linux, mac]
    steps:
      - run: echo hi
"#;
        let wf = parse_workflow("ci.yml", text).unwrap();
        let jobs = wf
            .expand(GhaContext {
                sha: "s",
                git_ref: "refs/heads/main",
                repository: "o/r",
                event_name: "push",
            })
            .unwrap();
        assert_eq!(jobs.len(), 2);
        assert!(!jobs[0].fail_fast);
        assert_eq!(jobs[0].max_parallel, Some(2));
    }
}
