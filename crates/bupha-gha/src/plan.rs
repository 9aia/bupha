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
        /// `env:` on the `uses:` step — scoped to the synthesized
        /// script, like GitHub scopes step env to the step.
        env: Vec<(String, String)>,
    },
    /// `uses:` naming a composite action (`./local/path` or
    /// `owner/repo[/path]@ref`) — the resolver fetched its
    /// `action.yml` at parse time; expansion inlines the action's
    /// steps with `inputs.*` bound from `with:`.
    Composite {
        id: Option<String>,
        cond: Option<String>,
        /// `timeout-minutes` on the uses step — a deadline spanning
        /// every inlined run step (rendered as per-step remaining-
        /// time `timeout` wrappers off one armed timestamp).
        timeout_seconds: Option<u64>,
        with: Vec<(String, Vec<String>)>,
        /// `env:` on the `uses:` step — applies to every inlined
        /// step, below each step's own `env:` (GitHub's scoping).
        env: Vec<(String, String)>,
        action: ActionDef,
    },
}

/// A resolved composite action's `action.yml`, parsed at workflow
/// parse time. `inputs` declare the `with:` contract (required +
/// defaults); `outputs` map output names to value templates
/// (`${{ steps.<inner-id>.outputs.<name> }}` style — interpolated
/// with the action's inputs bound, deferred refs survive for the
/// renderer); `steps` are ordinary workflow-shaped steps,
/// recursively parsed (nested composites included).
struct ActionDef {
    inputs: Vec<(String, ActionInput)>,
    outputs: Vec<(String, String)>,
    steps: Vec<RawStep>,
}

/// One declared `inputs.<name>` entry.
struct ActionInput {
    required: bool,
    default: Option<String>,
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
    /// `strategy.max-parallel` — caps concurrent matrix legs
    /// (default: all legs at once); `1` pins the sequential path.
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
    /// A composite action's output publisher — emitted after the
    /// inlined action steps when the `uses:` step declared an `id`.
    /// Writes each declared output's evaluated template into
    /// `stepouts/<job>/<uses-id>/` so `steps.<id>.outputs.*` and job
    /// `outputs:` resolve it, and records the uses-step's own
    /// conclusion under its id.
    Publish {
        id: String,
        cond: Option<Cond>,
        /// name → interpolated template; `steps.*`/`needs.*` refs
        /// inside stay deferred `${{ … }}` for the renderer.
        outputs: Vec<(String, String)>,
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
    /// Composite-`uses:` deadlines this step belongs to —
    /// `(deadline key, total seconds)` pairs. The renderer arms a
    /// timestamp file per key before the group's first step and
    /// wraps each run step's shell in `timeout <min remaining>`;
    /// a step can carry several (nested composites AND their
    /// deadlines — whichever fires first wins).
    pub deadlines: Vec<(String, u64)>,
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
#[derive(Debug, Clone)]
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
#[derive(Debug, Clone)]
pub enum DynRef {
    /// `needs.<job>.outputs.<name>` — the producer job's published
    /// output (highest-index successful matrix leg wins).
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
            inputs: None,
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
            inputs: None,
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
            steps.extend(build_steps(
                s,
                &eval,
                &job_env,
                ctx,
                &format!("{what}.steps[{si}]"),
            )?);
        }
        // Step ids — declared or composite-inlined — share one
        // `steps.<id>` namespace per job (a deliberate simplification
        // of GHA's per-action scoping): duplicates are ambiguous, so
        // they're an error like duplicate ids in one `steps:` list.
        let mut seen: HashSet<&str> = HashSet::new();
        for s in &steps {
            if let Some(id) = step_id(s) {
                if !seen.insert(id) {
                    return Err(format!(
                        "{what}: duplicate step id `{id}` (composite action steps share the job's step namespace)"
                    ));
                }
            }
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
    /// `inputs.*` — the composite action's bound inputs. `None`
    /// outside composite expansion; `Some` inside (possibly empty).
    pub inputs: Option<&'a [(String, String)]>,
}

/// The `id:` a built [`Step`] publishes under — `None` for
/// id-less steps (their `$GITHUB_OUTPUT` writes stay unreferenced).
fn step_id(s: &Step) -> Option<&str> {
    match s {
        Step::Run(r) => r.id.as_deref(),
        Step::Checkout { id, .. } => id.as_deref(),
        Step::Publish { id, .. } => Some(id),
    }
}

/// Expand one raw step into runnable [`Step`]s — usually exactly
/// one, but a composite `uses:` inlines the action's whole step
/// list (plus its output publisher).
fn build_steps(
    s: &RawStep,
    eval: &Eval,
    job_env: &[(String, String)],
    ctx: GhaContext,
    what: &str,
) -> Result<Vec<Step>, String> {
    if let RawStep::Composite {
        id,
        cond,
        timeout_seconds,
        with,
        env,
        action,
    } = s
    {
        return build_composite(
            CompositeCall {
                id,
                cond,
                timeout_seconds,
                with,
                env,
                action,
            },
            eval,
            job_env,
            ctx,
            what,
        );
    }
    build_single(s, eval, job_env, ctx, what).map(|st| vec![st])
}

/// AND two optional conditions — `None` is the identity (no gate).
fn and_conds(a: Option<Cond>, b: Option<Cond>) -> Option<Cond> {
    match (a, b) {
        (Some(x), Some(y)) => Some(Cond::Bin {
            op: BinOp::And,
            l: Box::new(x),
            r: Box::new(y),
        }),
        (Some(x), None) | (None, Some(x)) => Some(x),
        (None, None) => None,
    }
}

/// Interpolate a step's raw `with:` pairs — scalars stay one
/// entry, YAML lists flatten — under `eval`.
fn interpolate_with(
    with: &[(String, Vec<String>)],
    eval: &Eval,
    what: &str,
) -> Result<Vec<(String, Vec<String>)>, String> {
    let mut pairs = Vec::with_capacity(with.len());
    for (k, vals) in with {
        let mut out = Vec::with_capacity(vals.len());
        for v in vals {
            out.push(interpolate(v, eval, &format!("{what}.with.{k}"))?);
        }
        pairs.push((k.clone(), out));
    }
    Ok(pairs)
}

/// Bind a composite action's declared `inputs` against the
/// interpolated `with:` pairs: provided values win, declared
/// defaults fill gaps, `required` inputs without either are an
/// error, and `with:` keys the action doesn't declare are an
/// error (typo-safe, like GitHub's warning made loud).
fn bind_inputs(
    decls: &[(String, ActionInput)],
    provided: &[(String, Vec<String>)],
    what: &str,
) -> Result<Vec<(String, String)>, String> {
    for (k, vals) in provided {
        if !decls.iter().any(|(n, _)| n == k) {
            let known = decls
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>()
                .join(", ");
            return Err(format!(
                "{what}.with.{k}: the action declares no input `{k}`{}",
                if known.is_empty() {
                    String::new()
                } else {
                    format!(" — declared: {known}")
                }
            ));
        }
        if vals.len() != 1 {
            return Err(format!(
                "{what}.with.{k}: expected a single value, got {}",
                vals.len()
            ));
        }
    }
    let mut bound = Vec::with_capacity(decls.len());
    for (name, spec) in decls {
        let provided_val = provided
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v[0].clone());
        match (provided_val, &spec.default) {
            (Some(v), _) => bound.push((name.clone(), v)),
            (None, Some(d)) => bound.push((name.clone(), d.clone())),
            (None, None) if spec.required => {
                return Err(format!(
                    "{what}.with.{name}: missing required input `{name}`"
                ))
            }
            (None, None) => bound.push((name.clone(), String::new())),
        }
    }
    Ok(bound)
}

/// The `uses:`-step fields a [`RawStep::Composite`] carries —
/// bundled so [`build_composite`] stays under the arg-count lint.
struct CompositeCall<'a> {
    id: &'a Option<String>,
    cond: &'a Option<String>,
    timeout_seconds: &'a Option<u64>,
    with: &'a [(String, Vec<String>)],
    env: &'a [(String, String)],
    action: &'a ActionDef,
}

/// Expand a composite `uses:` step: bind `inputs.*` from `with:`,
/// inline the action's steps under a child eval, AND the uses-step's
/// own `if:` onto every inlined step (and the output publisher),
/// then append the [`Step::Publish`] that exposes the action's
/// declared `outputs` under the uses-step's `id`.
fn build_composite(
    call: CompositeCall<'_>,
    eval: &Eval,
    job_env: &[(String, String)],
    ctx: GhaContext,
    what: &str,
) -> Result<Vec<Step>, String> {
    let CompositeCall {
        id,
        cond,
        timeout_seconds,
        with,
        env,
        action,
    } = call;
    // `env:` on the uses step evaluates in the caller's context
    // (job env visible via env.*) and applies to every inlined
    // step — GitHub scopes it like a step env: above job env,
    // below each action step's own env.
    let uses_env = interpolate_pairs(env, eval, &format!("{what}.env"))?;
    let merged_env = merge_env(job_env, &uses_env);
    let step_eval = Eval {
        ctx,
        matrix: eval.matrix,
        env: Some(&merged_env),
        inputs: eval.inputs,
    };
    // `with:` values evaluate in the *caller's* context — the
    // parent's inputs (when nested), env, github, matrix — before
    // the action's own inputs are bound. env.* sees job+uses env,
    // matching how a run step's script sees its step env.
    let pairs = interpolate_with(with, &step_eval, what)?;
    let bound = bind_inputs(&action.inputs, &pairs, what)?;
    let child_eval = Eval {
        ctx,
        matrix: eval.matrix,
        env: Some(&merged_env),
        inputs: Some(&bound),
    };
    // The uses-step's `if:` gates the whole action — distribute it
    // over every inlined step (each keeps its own `if:` too, ANDed).
    let outer = cond
        .as_deref()
        .map(|raw| parse_cond(raw, &step_eval, &format!("{what}.if")))
        .transpose()?;
    let mut steps = Vec::with_capacity(action.steps.len() + 1);
    for (ai, sub) in action.steps.iter().enumerate() {
        let sub_what = format!("{what}.action.steps[{ai}]");
        let mut produced = build_steps(sub, &child_eval, &merged_env, ctx, &sub_what)?;
        if let Some(o) = &outer {
            for st in &mut produced {
                let gated = match st {
                    Step::Run(r) => and_conds(Some(o.clone()), r.cond.take()),
                    Step::Checkout { cond: c, .. } => and_conds(Some(o.clone()), c.take()),
                    Step::Publish { cond: c, .. } => and_conds(Some(o.clone()), c.take()),
                };
                match st {
                    Step::Run(r) => r.cond = gated,
                    Step::Checkout { cond: c, .. } => *c = gated,
                    Step::Publish { cond: c, .. } => *c = gated,
                }
            }
        }
        // The uses-step's env rides into every inlined run step,
        // under the step's own env (merge order = precedence).
        if !uses_env.is_empty() {
            for st in &mut produced {
                if let Step::Run(r) = st {
                    r.env = merge_env(&uses_env, &r.env);
                }
            }
        }
        steps.extend(produced);
    }
    // The uses-step's own handle: publish the action's declared
    // `outputs` (and record its conclusion) under its `id` — the
    // same `steps.<id>.{outputs,conclusion}` surface a `run:` step
    // gets. No `id` → outputs are unreachable, like on GitHub.
    if let Some(id) = id {
        let mut outputs = Vec::with_capacity(action.outputs.len());
        for (name, tmpl) in &action.outputs {
            outputs.push((
                name.clone(),
                interpolate(tmpl, &child_eval, &format!("{what}.outputs.{name}"))?,
            ));
        }
        steps.push(Step::Publish {
            id: id.clone(),
            cond: outer,
            outputs,
        });
    }
    // `timeout-minutes` on the uses step is a deadline spanning the
    // whole inlined group: tag every produced run step (nested
    // composites included — their own deadlines stack on top) with
    // this step's key; the renderer arms one timestamp and wraps
    // each step's shell in the remaining budget.
    if let Some(secs) = timeout_seconds {
        for st in &mut steps {
            if let Step::Run(r) = st {
                r.deadlines.push((what.to_string(), *secs));
            }
        }
    }
    Ok(steps)
}

fn build_single(
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
            env,
        } => {
            // Lower to a synthesized run step: interpolate the
            // `with:` inputs, then emit the action's bash. The
            // script itself contains no `${{ }}` — interpolation
            // already happened — so it goes through verbatim.
            // `env:` on the uses step scopes to this script (job
            // env < uses env), and `with:` sees it via env.* like
            // a run step's script sees its step env.
            let uses_env = interpolate_pairs(env, eval, &format!("{what}.env"))?;
            let merged_env = merge_env(job_env, &uses_env);
            let with_eval = Eval {
                ctx,
                matrix: eval.matrix,
                env: Some(&merged_env),
                inputs: eval.inputs,
            };
            let pairs = interpolate_with(with, &with_eval, what)?;
            let script = match kind {
                UsesScript::RustToolchain => rust_toolchain_script(&pairs, what)?,
                UsesScript::UploadArtifact => upload_artifact_script(&pairs, what)?,
                UsesScript::DownloadArtifact => download_artifact_script(&pairs, what)?,
                UsesScript::SetupNode => setup_node_script(&pairs, what)?,
                UsesScript::SetupPython => setup_python_script(&pairs, what)?,
                UsesScript::SetupGo => setup_go_script(&pairs, what)?,
                UsesScript::SetupJava => setup_java_script(&pairs, what)?,
                UsesScript::SetupDotnet => setup_dotnet_script(&pairs, what)?,
                UsesScript::CacheRestore => cache_restore_script(&pairs, what)?,
                UsesScript::CacheSave => cache_save_script(&pairs, what)?,
                UsesScript::Cache => cache_script(&pairs, what)?,
                UsesScript::CacheFlush => cache_flush_script(&pairs, what)?,
                UsesScript::Docker(image) => docker_script(image, &pairs, what)?,
            };
            Ok(Step::Run(RunStep {
                id: id.clone(),
                cond: cond
                    .as_deref()
                    .map(|raw| parse_cond(raw, &with_eval, &format!("{what}.if")))
                    .transpose()?,
                timeout_seconds: *timeout_seconds,
                deadlines: Vec::new(),
                shell: None,
                working_directory: None,
                env: uses_env,
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
                inputs: eval.inputs,
            };
            Ok(Step::Run(RunStep {
                id: id.clone(),
                cond: cond
                    .as_deref()
                    .map(|raw| parse_cond(raw, &step_eval, &format!("{what}.if")))
                    .transpose()?,
                timeout_seconds: *timeout_seconds,
                deadlines: Vec::new(),
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
        // `build_steps` intercepts composites before delegating —
        // reaching here would be a bug.
        RawStep::Composite { .. } => Err(format!(
            "{what}: internal error — composite step not expanded"
        )),
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

/// `echo` warnings for `with:` inputs that are valid on the real
/// action but have no local effect — emitted into the synthesized
/// script so the no-op is visible in logs. `ignored` is a fixed
/// list, so nothing user-controlled reaches the bash unquoted.
fn noop_warnings(pairs: &[(String, Vec<String>)], ignored: &[&str], action: &str) -> String {
    let mut s = String::new();
    for k in ignored {
        if pairs.iter().any(|(pk, _)| pk == k) {
            s.push_str(&format!(
                "echo \"gha: {action} — `{k}` accepted but is a no-op in the local lowering\"\n"
            ));
        }
    }
    s
}

/// A `*-version-file:` input must stay inside the worktree.
fn check_version_file(path: &str, key: &str, what: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.split('/').any(|c| c == "..")
        || path.chars().any(|c| c.is_control())
    {
        return Err(format!(
            "{what}.with.{key}: `{path}` must be a worktree-relative path"
        ));
    }
    Ok(())
}

/// Reject control characters in single-line `with:` values (cache
/// keys, version specs) — they'd break the synthesized bash or the
/// on-disk cache layout. `\n` is allowed for the multi-line `path`/
/// `restore-keys` blocks.
fn check_no_ctl(v: &str, key: &str, what: &str) -> Result<(), String> {
    if v.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!(
            "{what}.with.{key}: control characters are unsupported"
        ));
    }
    Ok(())
}

/// `actions/setup-node` lowered to bash: resolve `node-version`
/// (exact `x.y.z` or partial `x`/`x.y` → latest matching via
/// nodejs.org's index.json; `lts/*` and semver ranges error),
/// download the official linux tarball into the per-run toolcache
/// (`$BUPHA_GHA_TMP/toolcache/node/<ver>` — shared by later jobs in
/// the same run), and prepend its `bin/` via `$GITHUB_PATH`.
/// `node-version-file` reads `.nvmrc`-style files at run time.
/// npm-registry inputs and `cache` are accepted no-ops (no secrets,
/// no cross-run cache for npm here).
fn setup_node_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "node-version",
            "node-version-file",
            "architecture",
            "check-latest",
            "registry-url",
            "scope",
            "token",
            "always-auth",
            "cache",
            "cache-dependency-path",
        ],
        what,
    )?;
    let ver = with_one(pairs, "node-version", what)?;
    let vfile = with_one(pairs, "node-version-file", what)?;
    if ver.is_none() && vfile.is_none() {
        return Err(format!(
            "{what}.with: `node-version` or `node-version-file` is required"
        ));
    }
    if let Some(v) = ver {
        check_no_ctl(v, "node-version", what)?;
    }
    if let Some(v) = vfile {
        check_version_file(v, "node-version-file", what)?;
    }
    let arch = with_one(pairs, "architecture", what)?.unwrap_or("");
    check_no_ctl(arch, "architecture", what)?;

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &[
            "registry-url",
            "scope",
            "always-auth",
            "token",
            "cache",
            "cache-dependency-path",
        ],
        "setup-node",
    ));
    s.push_str(&format!("want={}\n", bash_sq(ver.unwrap_or(""))));
    s.push_str(&format!("vfile={}\n", bash_sq(vfile.unwrap_or(""))));
    s.push_str(&format!("arch={}\n", bash_sq(arch)));
    s.push_str(
        r#"if [ -z "$want" ]; then
  if [ -z "$vfile" ]; then
    echo "gha: setup-node — node-version or node-version-file is required"; exit 1
  fi
  if [ ! -f "$vfile" ]; then
    echo "gha: setup-node — version file '$vfile' not found"; exit 1
  fi
  want=$(grep -m1 -vE '^[[:space:]]*(#|$)' "$vfile" | head -1 | tr -d '[:space:]')
  want=${want#v}
fi
case "$want" in
  ''|.*|*.|*..*|*.*.*.*|*[!0-9.]*)
    echo "gha: setup-node — unsupported node-version '$want' (exact x.y.z or partial x / x.y numeric versions only; lts/* and ranges unsupported)"; exit 1 ;;
esac
dots=$(printf '%s' "$want" | tr -cd '.' | wc -c)
if [ "$dots" -eq 2 ]; then
  full="v$want"
else
  idx=$(curl -fsSL --max-time 30 https://nodejs.org/dist/index.json) || {
    echo "gha: setup-node — cannot fetch https://nodejs.org/dist/index.json"; exit 1; }
  full=$(printf '%s\n' "$idx" | grep -oE '"version":"v[0-9]+\.[0-9]+\.[0-9]+' | cut -d'"' -f4 | grep "^v${want}\." | head -1)
  [ -n "$full" ] || { echo "gha: setup-node — no Node.js '$want' release found"; exit 1; }
fi
if [ "$(uname -s)" != "Linux" ]; then
  echo "gha: setup-node — linux runners only"; exit 1
fi
a="$arch"
[ -z "$a" ] && a=$(uname -m)
case "$a" in x86_64) a=x64 ;; aarch64) a=arm64 ;; esac
case "$a" in
  x64|arm64) ;;
  *) echo "gha: setup-node — unsupported architecture '$a' (x64, arm64)"; exit 1 ;;
esac
tc="$BUPHA_GHA_TMP/toolcache/node/$full"
if [ ! -x "$tc/bin/node" ]; then
  mkdir -p "$tc"
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 300 "https://nodejs.org/dist/$full/node-$full-linux-$a.tar.xz" -o "$tmp/node.tar.xz"; then
    echo "gha: setup-node — download failed: node-$full-linux-$a.tar.xz"; rm -rf "$tmp"; exit 1
  fi
  tar -xJf "$tmp/node.tar.xz" -C "$tc" --strip-components=1
  rm -rf "$tmp"
fi
echo "$tc/bin" >> "$GITHUB_PATH"
printf 'node-version=%s\n' "${full#v}" >> "$GITHUB_OUTPUT"
echo "gha: setup-node — node $full (linux-$a) at $tc/bin"
"#,
    );
    Ok(s)
}

/// `actions/setup-go` lowered to bash: resolve `go-version` (exact
/// `x.y.z`, partial `x`/`x.y` → latest via go.dev's `?mode=json`,
/// `stable`/`latest` → newest; `tip` errors — it needs a source
/// build), download the official `go<ver>.linux-<arch>.tar.gz`
/// into the per-run toolcache, prepend `bin/` via `$GITHUB_PATH`
/// and publish `GOROOT` via `$GITHUB_ENV`.
fn setup_go_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "go-version",
            "go-version-file",
            "architecture",
            "check-latest",
            "cache",
            "cache-dependency-path",
            "token",
        ],
        what,
    )?;
    let ver = with_one(pairs, "go-version", what)?;
    let vfile = with_one(pairs, "go-version-file", what)?;
    if ver.is_none() && vfile.is_none() {
        return Err(format!(
            "{what}.with: `go-version` or `go-version-file` is required"
        ));
    }
    if let Some(v) = ver {
        check_no_ctl(v, "go-version", what)?;
    }
    if let Some(v) = vfile {
        check_version_file(v, "go-version-file", what)?;
    }
    let arch = with_one(pairs, "architecture", what)?.unwrap_or("");
    check_no_ctl(arch, "architecture", what)?;

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &["cache", "cache-dependency-path", "token"],
        "setup-go",
    ));
    s.push_str(&format!("want={}\n", bash_sq(ver.unwrap_or(""))));
    s.push_str(&format!("vfile={}\n", bash_sq(vfile.unwrap_or(""))));
    s.push_str(&format!("arch={}\n", bash_sq(arch)));
    s.push_str(
        r#"if [ -z "$want" ]; then
  if [ -z "$vfile" ]; then
    echo "gha: setup-go — go-version or go-version-file is required"; exit 1
  fi
  if [ ! -f "$vfile" ]; then
    echo "gha: setup-go — version file '$vfile' not found"; exit 1
  fi
  want=$(sed -n 's/^[[:space:]]*go[[:space:]][[:space:]]*\([^[:space:]]*\).*/\1/p' "$vfile" | head -1)
  [ -n "$want" ] || want=$(sed -n 's/^[[:space:]]*toolchain[[:space:]][[:space:]]*go\([^[:space:]]*\).*/\1/p' "$vfile" | head -1)
fi
want=${want#go}
case "$want" in
  stable|latest) ;;
  tip) echo "gha: setup-go — 'tip' needs a source build; unsupported"; exit 1 ;;
  ''|.*|*.|*..*|*.*.*.*|*[!0-9.]*)
    echo "gha: setup-go — unsupported go-version '$want' (exact x.y.z or partial x / x.y numeric versions, or stable/latest)"; exit 1 ;;
esac
dots=$(printf '%s' "$want" | tr -cd '.' | wc -c)
if [ "$dots" -eq 2 ]; then
  full="go$want"
else
  list=$(curl -fsSL --max-time 30 'https://go.dev/dl/?mode=json') || {
    echo "gha: setup-go — cannot fetch https://go.dev/dl/?mode=json"; exit 1; }
  vers=$(printf '%s\n' "$list" | grep -oE '"version":"go[0-9]+\.[0-9]+\.[0-9]+"' | cut -d'"' -f4 | sort -uV)
  case "$want" in
    stable|latest) full=$(printf '%s\n' "$vers" | tail -1) ;;
    *) full=$(printf '%s\n' "$vers" | grep "^go${want}\." | tail -1) ;;
  esac
  [ -n "$full" ] || { echo "gha: setup-go — no Go '$want' release found"; exit 1; }
fi
if [ "$(uname -s)" != "Linux" ]; then
  echo "gha: setup-go — linux runners only"; exit 1
fi
a="$arch"
[ -z "$a" ] && a=$(uname -m)
case "$a" in x86_64|amd64) a=amd64 ;; i686|i386|x86) a=386 ;; aarch64|arm64) a=arm64 ;; armv7l|armv6l|arm) a=arm ;; esac
case "$a" in
  amd64|arm64|386|arm) ;;
  *) echo "gha: setup-go — unsupported architecture '$a'"; exit 1 ;;
esac
tc="$BUPHA_GHA_TMP/toolcache/go/$full"
if [ ! -x "$tc/go/bin/go" ]; then
  mkdir -p "$tc"
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 300 "https://go.dev/dl/$full.linux-$a.tar.gz" -o "$tmp/go.tar.gz"; then
    echo "gha: setup-go — download failed: $full.linux-$a.tar.gz"; rm -rf "$tmp"; exit 1
  fi
  tar -xzf "$tmp/go.tar.gz" -C "$tc"
  rm -rf "$tmp"
fi
root="$tc/go"
echo "$root/bin" >> "$GITHUB_PATH"
echo "GOROOT=$root" >> "$GITHUB_ENV"
printf 'go-version=%s\n' "${full#go}" >> "$GITHUB_OUTPUT"
echo "gha: setup-go — go $full (linux-$a) at $root"
"#,
    );
    Ok(s)
}

/// `actions/setup-python` lowered to bash: resolve `python-version`
/// (exact `x.y.z` or partial `x`/`x.y` → latest matching) against
/// `actions/python-versions` git tags via `git ls-remote` (one call,
/// no API rate limit), pick the newest matching release, then probe
/// the linux distro builds (24.04 → 22.04 → 26.04 → rhel-10 →
/// rhel-9) for a downloadable asset. Installs into the per-run
/// toolcache; `update-environment` (default true) controls the
/// `$GITHUB_PATH` prepend, the `Python*_ROOT_DIR`/`LD_LIBRARY_PATH`
/// env writes happen either way.
fn setup_python_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "python-version",
            "python-version-file",
            "architecture",
            "check-latest",
            "token",
            "cache",
            "cache-dependency-path",
            "update-environment",
        ],
        what,
    )?;
    let ver = with_one(pairs, "python-version", what)?;
    let vfile = with_one(pairs, "python-version-file", what)?;
    if ver.is_none() && vfile.is_none() {
        return Err(format!(
            "{what}.with: `python-version` or `python-version-file` is required"
        ));
    }
    if let Some(v) = ver {
        check_no_ctl(v, "python-version", what)?;
    }
    if let Some(v) = vfile {
        check_version_file(v, "python-version-file", what)?;
    }
    let arch = with_one(pairs, "architecture", what)?.unwrap_or("");
    check_no_ctl(arch, "architecture", what)?;
    let updenv = with_flag(pairs, "update-environment", what)?.unwrap_or(true);

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &["token", "cache", "cache-dependency-path"],
        "setup-python",
    ));
    s.push_str(&format!("want={}\n", bash_sq(ver.unwrap_or(""))));
    s.push_str(&format!("vfile={}\n", bash_sq(vfile.unwrap_or(""))));
    s.push_str(&format!("arch={}\n", bash_sq(arch)));
    s.push_str(&format!("updenv={}\n", if updenv { "1" } else { "0" }));
    s.push_str(
        r#"if [ -z "$want" ]; then
  if [ -z "$vfile" ]; then
    echo "gha: setup-python — python-version or python-version-file is required"; exit 1
  fi
  if [ ! -f "$vfile" ]; then
    echo "gha: setup-python — version file '$vfile' not found"; exit 1
  fi
  want=$(grep -m1 -vE '^[[:space:]]*(#|$)' "$vfile" | head -1 | tr -d '[:space:]')
  want=${want#python}
  want=${want#py}
  want=${want#-}
fi
case "$want" in
  ''|.*|*.|*..*|*.*.*.*|*[!0-9.-]*)
    echo "gha: setup-python — unsupported python-version '$want' (exact x.y.z or partial x / x.y numeric versions only; pypy/pyenv specs unsupported)"; exit 1 ;;
esac
if [ "$(uname -s)" != "Linux" ]; then
  echo "gha: setup-python — linux runners only"; exit 1
fi
a="$arch"
[ -z "$a" ] && a=$(uname -m)
case "$a" in x86_64|amd64) a=x64 ;; aarch64|arm64) a=arm64 ;; *) echo "gha: setup-python — unsupported architecture '$a' (x64, arm64)"; exit 1 ;; esac
# Resolve the release tag: actions/python-versions tags look like
# <version>-<buildid> (e.g. 3.12.10-40336797752); the asset name
# uses the bare version. Partial specs match the newest stable.
dots=$(printf '%s' "$want" | tr -cd '.' | wc -c)
case "$dots" in
  0) pat="^${want}\.[0-9]+\.[0-9]+$" ;;
  1) pat="^${want}\.[0-9]+$" ;;
  *) pat="^${want}$" ;;
esac
refs=$(git ls-remote --tags https://github.com/actions/python-versions 2>/dev/null) || {
  echo "gha: setup-python — cannot reach github.com (git ls-remote failed)"; exit 1; }
match=$(printf '%s\n' "$refs" | sed -n 's|^[0-9a-f]*[[:space:]]\+refs/tags/\(.*\)$|\1|p' | grep -v '\^{}$' | while IFS= read -r tag; do
  ver=${tag%-[0-9]*}
  case "$ver" in [0-9]*) ;; *) continue ;; esac
  if [ "$dots" -lt 2 ] && [ "${ver#*-}" != "$ver" ]; then continue; fi
  printf '%s' "$ver" | grep -qE "$pat" || continue
  printf '%s %s\n' "$ver" "$tag"
done | sort -k1,1V -k2,2V | tail -1)
[ -n "$match" ] || { echo "gha: setup-python — no CPython '$want' release found"; exit 1; }
ver=${match%% *}
tag=${match##* }
tc="$BUPHA_GHA_TMP/toolcache/python/$ver"
distro=""
if [ ! -x "$tc/bin/python3" ] && [ ! -x "$tc/bin/python" ]; then
  mkdir -p "$tc"
  tmp=$(mktemp -d)
  for d in 24.04 22.04 26.04 rhel-10 rhel-9; do
    url="https://github.com/actions/python-versions/releases/download/$tag/python-$ver-linux-$d-$a.tar.gz"
    if curl -fsSL --max-time 300 "$url" -o "$tmp/py.tar.gz"; then distro=$d; break; fi
  done
  if [ -z "$distro" ]; then
    echo "gha: setup-python — no linux-$a build of python $ver"; rm -rf "$tmp"; exit 1
  fi
  tar -xzf "$tmp/py.tar.gz" -C "$tc" --strip-components=1
  rm -rf "$tmp"
fi
if [ "$updenv" = "1" ]; then echo "$tc/bin" >> "$GITHUB_PATH"; fi
{
  echo "Python_ROOT_DIR=$tc"
  echo "Python2_ROOT_DIR=$tc"
  echo "Python3_ROOT_DIR=$tc"
  echo "LD_LIBRARY_PATH=$tc/lib"
} >> "$GITHUB_ENV"
printf 'python-version=%s\n' "$ver" >> "$GITHUB_OUTPUT"
echo "gha: setup-python — python $ver (linux-${distro:-cached}-$a) at $tc"
"#,
    );
    Ok(s)
}

/// `actions/setup-java` lowered to bash: resolve `java-version`
/// (major `x` → `binary/latest/<x>`; partial `x.y`/exact `x.y.z[+b]`
/// → `info/release_names` prefix match + `binary/version/<name>` —
/// both 307 to the Temurin tarball; `jdkFile` extracts a
/// workspace-local tarball instead) and unpack into the per-run
/// toolcache (`$BUPHA_GHA_TMP/toolcache/java/<release>`).
/// `JAVA_HOME` goes to
/// `$GITHUB_ENV`, `bin/` to `$GITHUB_PATH`. `distribution` is
/// allowlisted to temurin (`adopt`/`adoptopenjdk` are aliases);
/// other vendors hard-error at plan time. Maven `settings.*`,
/// `server-*`, `gpg-*`, `mvn-toolchain-*`, `check-latest`, `token`
/// and `cache*` inputs are accepted no-ops — no Maven settings.xml
/// or cross-run cache in the local lowering.
fn setup_java_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "java-version",
            "java-version-file",
            "distribution",
            "java-package",
            "architecture",
            "jdkFile",
            "check-latest",
            "server-id",
            "server-username",
            "server-password",
            "settings-path",
            "overwrite-settings",
            "gpg-private-key",
            "gpg-passphrase",
            "cache",
            "cache-dependency-path",
            "job-status",
            "token",
            "mvn-toolchain-id",
            "mvn-toolchain-vendor",
        ],
        what,
    )?;
    let ver = with_one(pairs, "java-version", what)?;
    let vfile = with_one(pairs, "java-version-file", what)?;
    let jdkfile = with_one(pairs, "jdkFile", what)?;
    if ver.is_none() && vfile.is_none() && jdkfile.is_none() {
        return Err(format!(
            "{what}.with: `java-version`/`java-version-file` or `jdkFile` is required"
        ));
    }
    if let Some(v) = ver {
        check_no_ctl(v, "java-version", what)?;
    }
    if let Some(v) = vfile {
        check_version_file(v, "java-version-file", what)?;
    }
    if let Some(v) = jdkfile {
        check_version_file(v, "jdkFile", what)?;
    }
    let dist = with_one(pairs, "distribution", what)?.unwrap_or("temurin");
    match dist.to_ascii_lowercase().as_str() {
        "temurin" | "adopt" | "adoptopenjdk" => {}
        other => {
            return Err(format!(
                "{what}.with.distribution: `{other}` is unsupported — the v1 \
                 lowering supports temurin only (adopt/adoptopenjdk alias; \
                 zulu/corretto/oracle/semeru/… deferred)"
            ))
        }
    }
    let pkg = with_one(pairs, "java-package", what)?.unwrap_or("jdk");
    if !matches!(pkg, "jdk" | "jre") {
        return Err(format!(
            "{what}.with.java-package: expected `jdk` or `jre`, got `{pkg}`"
        ));
    }
    let arch = with_one(pairs, "architecture", what)?.unwrap_or("");
    check_no_ctl(arch, "architecture", what)?;

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &[
            "check-latest",
            "server-id",
            "server-username",
            "server-password",
            "settings-path",
            "overwrite-settings",
            "gpg-private-key",
            "gpg-passphrase",
            "cache",
            "cache-dependency-path",
            "job-status",
            "token",
            "mvn-toolchain-id",
            "mvn-toolchain-vendor",
        ],
        "setup-java",
    ));
    s.push_str(&format!("want={}\n", bash_sq(ver.unwrap_or(""))));
    s.push_str(&format!("vfile={}\n", bash_sq(vfile.unwrap_or(""))));
    s.push_str(&format!("jdkfile={}\n", bash_sq(jdkfile.unwrap_or(""))));
    s.push_str(&format!("arch={}\n", bash_sq(arch)));
    s.push_str(&format!("pkg={}\n", bash_sq(pkg)));
    s.push_str(
        r#"if [ -n "$jdkfile" ]; then
  # jdkFile — extract the workspace-local JDK archive, no API lookup.
  tc="$BUPHA_GHA_TMP/toolcache/java/jdkfile-$(printf '%s' "$jdkfile" | cksum | cut -d' ' -f1)"
  if [ ! -x "$tc/bin/java" ]; then
    [ -f "$jdkfile" ] || { echo "gha: setup-java — jdkFile '$jdkfile' not found"; exit 1; }
    mkdir -p "$tc"
    if ! tar -xzf "$jdkfile" -C "$tc" --strip-components=1 2>/dev/null; then
      echo "gha: setup-java — cannot extract jdkFile '$jdkfile' (expected a .tar.gz JDK)"; exit 1
    fi
  fi
  echo "JAVA_HOME=$tc" >> "$GITHUB_ENV"
  echo "$tc/bin" >> "$GITHUB_PATH"
  echo "gha: setup-java — JDK from '$jdkfile' at $tc"
  exit 0
fi
if [ -z "$want" ]; then
  if [ -z "$vfile" ]; then
    echo "gha: setup-java — java-version or java-version-file is required"; exit 1
  fi
  if [ ! -f "$vfile" ]; then
    echo "gha: setup-java — version file '$vfile' not found"; exit 1
  fi
  # .java-version / sdkman-style lines may carry a vendor or
  # build suffix ("temurin-21.0.7+6", "17.0.2-zulu") — keep the
  # first numeric semver token.
  want=$(grep -m1 -vE '^[[:space:]]*(#|$)' "$vfile" | head -1 | grep -oE '[0-9]+(\.[0-9]+){0,2}(\+[0-9]+)?' | head -1)
fi
case "$want" in
  ''|.*|*.|*..*|*.*.*.*.*|*[!0-9.+]*)
    echo "gha: setup-java — unsupported java-version '$want' (exact x.y.z[+build] or partial x / x.y numeric versions only)"; exit 1 ;;
esac
if [ "$(uname -s)" != "Linux" ]; then
  echo "gha: setup-java — linux runners only"; exit 1
fi
a="$arch"
[ -z "$a" ] && a=$(uname -m)
case "$a" in x86_64|amd64|x64) a=x64 ;; aarch64|arm64) a=aarch64 ;; esac
case "$a" in
  x64|aarch64) ;;
  *) echo "gha: setup-java — unsupported architecture '$a' (x64, aarch64)"; exit 1 ;;
esac
# Resolve to a release name + download URL. `binary/latest/<major>`
# 307-redirects to the newest GA build (the Location basename names
# the release); partial/full specs go through `info/release_names`
# (one JSON list, newest first) matched on a `.`/`+` boundary, then
# `binary/version/<jdk-x.y.z+build>`.
dots=$(printf '%s' "$want" | tr -cd '.' | wc -c)
if [ "$dots" -eq 0 ]; then
  loc=$(curl -fsSI --max-time 30 "https://api.adoptium.net/v3/binary/latest/$want/ga/linux/$a/$pkg/hotspot/normal/eclipse" | tr -d '\r' | sed -n 's/^[Ll]ocation: //p' | head -1)
  [ -n "$loc" ] || { echo "gha: setup-java — no Temurin $pkg for java-version '$want' (linux-$a)"; exit 1; }
  rel=${loc##*/}
  rel=${rel%.tar.gz}
  url=$loc
else
  names=$(curl -fsSL --max-time 30 'https://api.adoptium.net/v3/info/release_names?project=jdk&release_type=ga') || {
    echo "gha: setup-java — cannot reach api.adoptium.net"; exit 1; }
  rel=$(printf '%s\n' "$names" | grep -oE '"jdk-[0-9][^"]*"' | while IFS= read -r n; do
    v=${n#\"jdk-}; v=${v%\"}
    case "$v" in "$want"*) ;; *) continue ;; esac
    rest=${v#$want}
    case "$rest" in ""|+*|.*) printf '%s\n' "$n" ;; esac
  done | head -1)
  rel=${rel#\"}; rel=${rel%\"}
  [ -n "$rel" ] || { echo "gha: setup-java — no Temurin $pkg release matching java-version '$want'"; exit 1; }
  url="https://api.adoptium.net/v3/binary/version/${rel//+/%2B}/linux/$a/$pkg/hotspot/normal/eclipse"
fi
tc="$BUPHA_GHA_TMP/toolcache/java/$rel"
if [ ! -x "$tc/bin/java" ]; then
  mkdir -p "$tc"
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 600 "$url" -o "$tmp/jdk.tar.gz"; then
    echo "gha: setup-java — download failed: $url"; rm -rf "$tmp"; exit 1
  fi
  tar -xzf "$tmp/jdk.tar.gz" -C "$tc" --strip-components=1
  rm -rf "$tmp"
fi
echo "JAVA_HOME=$tc" >> "$GITHUB_ENV"
echo "$tc/bin" >> "$GITHUB_PATH"
echo "gha: setup-java — temurin $rel (linux-$a $pkg) at $tc"
"#,
    );
    Ok(s)
}

/// `actions/setup-dotnet` lowered to bash: fetch Microsoft's
/// `dotnet-install.sh` and install into the per-run toolcache
/// (`$BUPHA_GHA_TMP/toolcache/dotnet/<spec>`). `dotnet-version`
/// maps to `-Channel` (partial/wildcard specs → latest in the
/// channel) or `-Version` (full `x.y.z`); absent → `global.json`'s
/// `sdk.version` (repo root or `global-json-file`). `dotnet-quality`
/// passes through to `-Quality` (`ga` default; `signed`/`validated`
/// are NuGet-signature options the SDK installer doesn't take and
/// hard-error). NuGet-auth inputs (`source-url`, `owner`,
/// `config-file`), `cache*` and the `cache-hit`/`dotnet-version`
/// outputs' side effects are accepted no-ops — `dotnet-version` is
/// still published when resolvable.
fn setup_dotnet_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "dotnet-version",
            "dotnet-quality",
            "global-json-file",
            "source-url",
            "owner",
            "config-file",
            "cache",
            "cache-dependency-path",
        ],
        what,
    )?;
    let ver = with_one(pairs, "dotnet-version", what)?;
    let quality = with_one(pairs, "dotnet-quality", what)?.unwrap_or("ga");
    if !matches!(
        quality.to_ascii_lowercase().as_str(),
        "ga" | "daily" | "preview" | "all"
    ) {
        return Err(format!(
            "{what}.with.dotnet-quality: `{quality}` unsupported — \
             the SDK installer takes ga/daily/preview/all \
             (signed/validated are NuGet-signature options)"
        ));
    }
    let gjfile = with_one(pairs, "global-json-file", what)?;
    if let Some(v) = ver {
        check_no_ctl(v, "dotnet-version", what)?;
    }
    if let Some(v) = gjfile {
        check_version_file(v, "global-json-file", what)?;
    }

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &[
            "source-url",
            "owner",
            "config-file",
            "cache",
            "cache-dependency-path",
        ],
        "setup-dotnet",
    ));
    s.push_str(&format!("want={}\n", bash_sq(ver.unwrap_or(""))));
    s.push_str(&format!("quality={}\n", bash_sq(quality)));
    s.push_str(&format!("gjfile={}\n", bash_sq(gjfile.unwrap_or(""))));
    s.push_str(
        r#"q=$(printf '%s' "$quality" | tr '[:lower:]' '[:upper:]')
case "$q" in GA) q=ga ;; DAILY) q=daily ;; PREVIEW) q=preview ;; ALL) q=all ;; esac
if [ -z "$want" ]; then
  gf="$gjfile"
  [ -z "$gf" ] && [ -f global.json ] && gf=global.json
  if [ -n "$gf" ]; then
    [ -f "$gf" ] || { echo "gha: setup-dotnet — global-json-file '$gf' not found"; exit 1; }
    want=$(grep -oE '"version"[[:space:]]*:[[:space:]]*"[^"]+"' "$gf" | head -1 | sed -E 's/.*"([^"]+)"[[:space:]]*$/\1/')
  fi
fi
[ -n "$want" ] || { echo "gha: setup-dotnet — dotnet-version or a global.json sdk.version is required"; exit 1; }
case "$want" in
  *[!0-9.xX]*)
    echo "gha: setup-dotnet — unsupported dotnet-version '$want' (numeric x[.y[.z]] or channel specs like 8.0.x only)"; exit 1 ;;
esac
# Full x.y.z → -Version pin; partial/wildcard → -Channel major.minor.
dots=$(printf '%s' "$want" | tr -cd '.' | wc -c)
case "$want" in
  *[xX]*) iswild=1 ;;
  *) iswild=0 ;;
esac
if [ "$iswild" = "1" ] || [ "$dots" -lt 2 ]; then
  majmin=$(printf '%s' "$want" | grep -oE '^[0-9]+(\.[0-9]+)?' | head -1)
  case "$majmin" in *.*) ;; *) majmin="$majmin.0" ;; esac
  sel=(-Channel "$majmin")
else
  sel=(-Version "$want")
fi
dir="$BUPHA_GHA_TMP/toolcache/dotnet/${want//[^A-Za-z0-9._-]/_}"
if [ ! -x "$dir/dotnet" ]; then
  tmp=$(mktemp -d)
  if ! curl -fsSL --max-time 120 https://dot.net/v1/dotnet-install.sh -o "$tmp/install.sh"; then
    echo "gha: setup-dotnet — cannot fetch https://dot.net/v1/dotnet-install.sh"; rm -rf "$tmp"; exit 1
  fi
  mkdir -p "$dir"
  if ! bash "$tmp/install.sh" --install-dir "$dir" -Quality "$q" "${sel[@]}"; then
    rm -rf "$tmp"; echo "gha: setup-dotnet — dotnet-install failed for '$want' (quality $q)"; exit 1
  fi
  rm -rf "$tmp"
fi
echo "$dir" >> "$GITHUB_PATH"
echo "DOTNET_ROOT=$dir" >> "$GITHUB_ENV"
ver_out=$("$dir/dotnet" --version 2>/dev/null || true)
[ -n "$ver_out" ] && printf 'dotnet-version=%s\n' "$ver_out" >> "$GITHUB_OUTPUT"
echo "gha: setup-dotnet — dotnet $ver_out at $dir"
"#,
    );
    Ok(s)
}

/// Bash helpers shared by the `actions/cache*` lowerings: the
/// persistent store root (`$BUPHA_GHA_CACHE_DIR` override →
/// `$XDG_DATA_HOME`/`~/.local/share` → `bupha/gha-cache`), one
/// directory per repo, one `<sha256(key)>.tar` + `<sha256(key)>.key`
/// pair per cache entry so exact lookups hash the key while
/// `restore-keys` prefix scans read the `.key` files.
const CACHE_LIB: &str = r#"
bupha_cache_repo_dir() {
  local root="${BUPHA_GHA_CACHE_DIR:-${XDG_DATA_HOME:-$HOME/.local/share}/bupha/gha-cache}"
  printf '%s/%s' "$root" "$(printf '%s' "$GITHUB_REPOSITORY" | tr -c 'A-Za-z0-9._-' '_')"
}

# $1 = key, $2 = newline-separated paths block, $3 = strict
# (1 = error when the key already exists, like actions/cache/save).
bupha_cache_save() {
  local skey="$1" supaths="$2" strict="$3"
  local repo_dir kh star skeyf tmp _e _m
  repo_dir=$(bupha_cache_repo_dir)
  kh=$(printf '%s' "$skey" | sha256sum | cut -d' ' -f1)
  star="$repo_dir/$kh.tar"
  skeyf="$repo_dir/$kh.key"
  if [ -e "$star" ]; then
    if [ "$strict" = "1" ]; then
      echo "gha: cache/save — key '$skey' already exists"; return 1
    fi
    echo "gha: cache — key '$skey' already cached; skipping save"; return 0
  fi
  mkdir -p "$repo_dir"
  shopt -s dotglob nullglob
  local -a _matches=() _uniq=()
  local -A _seen=()
  while IFS= read -r _e; do
    _e="${_e#"${_e%%[![:space:]]*}"}"
    _e="${_e%"${_e##*[![:space:]]}"}"
    [ -z "$_e" ] && continue
    case "$_e" in
      "$GITHUB_WORKSPACE"/*) _e="${_e#"$GITHUB_WORKSPACE"/}" ;;
      /*) echo "gha: cache — absolute path '$_e' is unsupported (paths are workspace-relative)"; return 1 ;;
    esac
    while IFS= read -r _m; do _matches+=("$_m"); done < <(compgen -G "$_e")
  done < <(printf '%s\n' "$supaths")
  if [ "${#_matches[@]}" -eq 0 ]; then
    echo "gha: cache — no files matched the given path(s); nothing saved"; return 0
  fi
  for _m in "${_matches[@]}"; do
    if [ -z "${_seen[$_m]:-}" ]; then _seen[$_m]=1; _uniq+=("$_m"); fi
  done
  tmp=$(mktemp -d)
  if ! tar -cf "$tmp/c.tar" -C "$GITHUB_WORKSPACE" -- "${_uniq[@]}"; then
    rm -rf "$tmp"; return 1
  fi
  mv "$tmp/c.tar" "$star"
  printf '%s' "$skey" > "$skeyf"
  rm -rf "$tmp"
  echo "gha: cache — saved '$skey' (${#_uniq[@]} path(s)) under $repo_dir"
}

# $1 = key, $2 = newline-separated restore-keys, $3 = lookup-only
# (1 = report the hit without extracting), $4 = fail-on-cache-miss.
bupha_cache_restore() {
  local key="$1" rkeys="$2" lookup="$3" failmiss="$4"
  local repo_dir kh hit="" hitkey="" exact=0 rk kf ck ct cm best bestm bestkey hitout
  repo_dir=$(bupha_cache_repo_dir)
  kh=$(printf '%s' "$key" | sha256sum | cut -d' ' -f1)
  if [ -f "$repo_dir/$kh.tar" ] && [ -f "$repo_dir/$kh.key" ]; then
    hit="$repo_dir/$kh.tar"; hitkey=$(cat "$repo_dir/$kh.key"); exact=1
  else
    while IFS= read -r rk; do
      rk="${rk#"${rk%%[![:space:]]*}"}"
      rk="${rk%"${rk##*[![:space:]]}"}"
      [ -n "$rk" ] || continue
      best=""; bestm=0; bestkey=""
      for kf in "$repo_dir"/*.key; do
        [ -f "$kf" ] || continue
        ck=$(cat "$kf")
        case "$ck" in "$rk"*) ;; *) continue ;; esac
        ct="${kf%.key}.tar"; [ -f "$ct" ] || continue
        cm=$(stat -c %Y "$ct" 2>/dev/null); cm=${cm:-0}
        if [ "$cm" -gt "$bestm" ]; then bestm=$cm; best="$ct"; bestkey="$ck"; fi
      done
      if [ -n "$best" ]; then hit="$best"; hitkey="$bestkey"; break; fi
    done < <(printf '%s\n' "$rkeys")
  fi
  if [ -n "$hit" ]; then
    if [ "$lookup" != "1" ]; then
      tar -xf "$hit" -C "$GITHUB_WORKSPACE" || {
        echo "gha: cache — restore extract failed for '$hitkey'"; return 1; }
    fi
    echo "gha: cache — restored key '${hitkey:-$key}'"
  else
    echo "gha: cache — miss for key '$key'"
  fi
  hitout=false
  [ "$exact" = "1" ] && hitout=true
  printf 'cache-hit=%s\ncache-primary-key=%s\n' "$hitout" "${hitkey:-$key}" >> "$GITHUB_OUTPUT"
  if [ "$failmiss" = "1" ] && [ -z "$hit" ]; then
    echo "gha: cache — fail-on-cache-miss and no cache matched"; return 1
  fi
}
"#;

/// Shared `with:` validation for the cache actions: `key` and `path`
/// are required single values; `restore-keys` flattens to a
/// newline-separated block (each YAML-list entry or line is one
/// prefix). Control characters other than `\n` are rejected.
fn cache_key_path(
    pairs: &[(String, Vec<String>)],
    what: &str,
) -> Result<(String, String, String), String> {
    let key = with_one(pairs, "key", what)?
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{what}.with.key: required"))?;
    check_no_ctl(key, "key", what)?;
    // `path` accepts a multiline string or a YAML list — both flatten
    // to the same newline-separated block the bash expands.
    let path = pairs
        .iter()
        .find(|(k, _)| k == "path")
        .map(|(_, v)| v.join("\n"))
        .map(|p| p.trim().to_string())
        .filter(|s| !s.is_empty())
        .ok_or_else(|| format!("{what}.with.path: required"))?;
    check_no_ctl(&path, "path", what)?;
    let rkeys = pairs
        .iter()
        .find(|(k, _)| k == "restore-keys")
        .map(|(_, v)| v.join("\n"))
        .unwrap_or_default();
    check_no_ctl(&rkeys, "restore-keys", what)?;
    Ok((key.to_string(), path.to_string(), rkeys))
}

/// `actions/cache/restore` → `bupha_cache_restore`: exact key hit,
/// else `restore-keys` prefix match (first matching prefix wins,
/// newest entry inside it), extract into the workspace root.
/// `lookup-only` reports the hit without extracting;
/// `fail-on-cache-miss` turns a miss into a step failure. Emits the
/// `cache-hit`/`cache-primary-key` step outputs.
fn cache_restore_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &[
            "key",
            "path",
            "restore-keys",
            "enableCrossOsArchive",
            "fail-on-cache-miss",
            "lookup-only",
        ],
        what,
    )?;
    let (key, _path, rkeys) = cache_key_path(pairs, what)?;
    let lookup = with_flag(pairs, "lookup-only", what)?.unwrap_or(false);
    let failmiss = with_flag(pairs, "fail-on-cache-miss", what)?.unwrap_or(false);

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &["enableCrossOsArchive"],
        "cache/restore",
    ));
    s.push_str(CACHE_LIB);
    s.push_str(&format!(
        "bupha_cache_restore {} {} {} {}\n",
        bash_sq(&key),
        bash_sq(&rkeys),
        if lookup { "1" } else { "0" },
        if failmiss { "1" } else { "0" },
    ));
    Ok(s)
}

/// `actions/cache/save` → `bupha_cache_save` strict: tars the
/// `path` globs into the persistent store under `key`; errors when
/// the key already exists (GitHub's immutable-cache rule).
fn cache_save_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &["key", "path", "upload-chunk-size", "enableCrossOsArchive"],
        what,
    )?;
    let (key, path, _) = cache_key_path(pairs, what)?;

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(
        pairs,
        &["upload-chunk-size", "enableCrossOsArchive"],
        "cache/save",
    ));
    s.push_str(CACHE_LIB);
    s.push_str(&format!(
        "bupha_cache_save {} {} 1\n",
        bash_sq(&key),
        bash_sq(&path),
    ));
    Ok(s)
}

/// `actions/cache` → restore now, save at job end. GitHub does the
/// save in a post-step; the lowering registers a pending spec under
/// `$RUNNER_TEMP/bupha-cache-pending/` and the runner-appended
/// `CacheFlush` step (end of the job's steps) performs it — so
/// files produced by later steps are captured. A job that fails
/// before its last step saves nothing (documented divergence: the
/// real post-step runs even on failure).
fn cache_script(pairs: &[(String, Vec<String>)], what: &str) -> Result<String, String> {
    check_with_keys(
        pairs,
        &["key", "path", "restore-keys", "enableCrossOsArchive"],
        what,
    )?;
    let (key, path, rkeys) = cache_key_path(pairs, what)?;

    let mut s = String::from("set -e\n");
    s.push_str(&noop_warnings(pairs, &["enableCrossOsArchive"], "cache"));
    s.push_str(CACHE_LIB);
    s.push_str(&format!(
        "bupha_cache_restore {} {} 0 0\n",
        bash_sq(&key),
        bash_sq(&rkeys),
    ));
    s.push_str(&format!("key={}\n", bash_sq(&key)));
    s.push_str(&format!("upaths={}\n", bash_sq(&path)));
    s.push_str(
        "pend=\"$RUNNER_TEMP/bupha-cache-pending\"\n\
         mkdir -p \"$pend\"\n\
         printf '%s\\n%s' \"$key\" \"$upaths\" > \"$pend/$(date +%s%N)-$RANDOM\"\n",
    );
    s.push_str("echo \"gha: cache — restore done; save of the key scheduled at job end\"\n");
    Ok(s)
}

/// The job-end step `parse_job` appends when a job uses
/// `actions/cache`: drains every pending spec the job's cache steps
/// wrote and saves each (non-strict — an already-existing key, e.g.
/// an exact-hit restore, is skipped like GitHub's post-step).
fn cache_flush_script(_pairs: &[(String, Vec<String>)], _what: &str) -> Result<String, String> {
    Ok(format!(
        "set -e\n\
         pend=\"$RUNNER_TEMP/bupha-cache-pending\"\n\
         [ -d \"$pend\" ] || exit 0\n{CACHE_LIB}\
         for spec in \"$pend\"/*; do\n\
         \x20 [ -f \"$spec\" ] || continue\n\
         \x20 skey=$(head -n 1 \"$spec\")\n\
         \x20 supaths=$(tail -n +2 \"$spec\")\n\
         \x20 bupha_cache_save \"$skey\" \"$supaths\" 0\n\
         done\n\
         echo \"gha: cache — pending saves flushed\"\n"
    ))
}

/// `uses: docker://<image>` lowered to bash: `docker run` of the
/// image with the workspace + `$BUPHA_GHA_TMP` bind-mounted at
/// their host paths (so `$GITHUB_*` file commands and
/// `$GITHUB_OUTPUT`/`$GITHUB_ENV` writes round-trip through the
/// mount), the full exported environment passed via `--env-file`,
/// and `with.entrypoint`/`with.args` mapped to `--entrypoint` +
/// argv. Honest boundaries: the docker CLI must exist on the
/// runner host (checked at run time, not plan time — plan stays
/// I/O-free), the image is pulled by docker's default policy,
/// there is no registry-credential plumbing (`with:` has no
/// username/password on GitHub's docker steps either — those live
/// on `jobs.<id>.container`/`services`, still unsupported), and
/// env values containing newlines can't survive `--env-file`'s
/// line format. `--rm --init` plus a named container + EXIT trap
/// keep a killed/timed-out step from leaking containers.
fn docker_script(
    image: &str,
    pairs: &[(String, Vec<String>)],
    what: &str,
) -> Result<String, String> {
    // The image lands as a quoted argv word, but a leading `-`
    // would still be parsed as a flag by some docker versions —
    // and whitespace/control can't appear in a real ref anyway.
    if image.is_empty()
        || image.starts_with('-')
        || image.chars().any(|c| c.is_whitespace() || c.is_control())
    {
        return Err(format!("{what}: invalid docker image `{image}`"));
    }
    check_with_keys(pairs, &["entrypoint", "args"], what)?;
    let entrypoint = with_one(pairs, "entrypoint", what)?.map(str::to_string);
    let mut args = String::new();
    if let Some((_, vals)) = pairs.iter().find(|(k, _)| k == "args") {
        // Each `with.args` list entry is one argv element (a
        // scalar `args:` string is one element too — GitHub's
        // docker steps take a list).
        for v in vals {
            if !args.is_empty() {
                args.push(' ');
            }
            args.push_str(&bash_sq(v));
        }
    }
    let ep = entrypoint
        .map(|e| format!("_docker_ep=(--entrypoint {})\n", bash_sq(&e)))
        .unwrap_or_else(|| "_docker_ep=()\n".to_string());
    Ok(format!(
        "set -e\n\
         command -v docker >/dev/null 2>&1 || {{ echo \"gha: docker step — docker CLI not found on PATH\"; exit 1; }}\n\
         _docker_env=\"$RUNNER_TEMP/docker_env\"\n\
         env > \"$_docker_env\" || {{ echo \"gha: docker step — cannot write env file\"; exit 1; }}\n\
         _cid=\"bupha-docker-$$-$RANDOM\"\n\
         trap 'docker rm -f \"$_cid\" >/dev/null 2>&1 || :; rm -f \"$_docker_env\"' EXIT\n\
         _docker_mounts=(-v \"$BUPHA_GHA_TMP:$BUPHA_GHA_TMP\")\n\
         case \"$GITHUB_WORKSPACE\" in\n\
         \x20 \"$BUPHA_GHA_TMP\"|\"$BUPHA_GHA_TMP\"/*) ;;\n\
         \x20 *) _docker_mounts+=(-v \"$GITHUB_WORKSPACE:$GITHUB_WORKSPACE\") ;;\n\
         esac\n\
         {ep}\
         _docker_args=({args})\n\
         docker run --rm --init --name \"$_cid\" --env-file \"$_docker_env\" \\\n\x20 \"${{_docker_mounts[@]}}\" -w \"$PWD\" \"${{_docker_ep[@]}}\" {} \"${{_docker_args[@]}}\"\n",
        bash_sq(image),
    ))
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
             inputs.* (inside composite actions), \
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
        "inputs" => {
            if rest.is_empty() {
                return Err(bad());
            }
            let Some(inputs) = eval.inputs else {
                return Err(format!(
                    "{what}: `inputs.*` is only available inside composite actions"
                ));
            };
            // Undeclared/unprovided inputs expand empty, like env.*.
            Ok(inputs
                .iter()
                .rev()
                .find(|(k, _)| k == rest)
                .map(|(_, v)| v.clone())
                .unwrap_or_default())
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

/// Resolves a `uses:` target that names an action directory —
/// `./local/path` or `owner/repo[/sub/path]@ref` — to the action's
/// `action.yml` *text*. plan stays I/O-free: the runner implements
/// this (filesystem reads for `./`, git fetch for remote refs);
/// [`parse_workflow`] uses [`NoActions`], which errors on any
/// action `uses:`.
pub trait ActionResolver {
    fn action_yaml(&self, uses: &str) -> Result<String, String>;
}

/// Resolver for [`parse_workflow`] — no action fetching, so every
/// `./`/remote `uses:` is an explicit error.
struct NoActions;

impl ActionResolver for NoActions {
    fn action_yaml(&self, uses: &str) -> Result<String, String> {
        Err(format!(
            "`uses: {uses}` names an action — this parse context has no action resolver"
        ))
    }
}

/// Nesting cap for composite actions calling composite actions —
/// GitHub has no documented limit; 8 is far past real usage and
/// keeps a cyclic action repo from recursing forever at parse.
const MAX_ACTION_DEPTH: usize = 8;

/// Parse + validate one workflow file. `source` names the file in
/// errors. Composite/local/remote action `uses:` fail — use
/// [`parse_workflow_with`] with an [`ActionResolver`].
pub fn parse_workflow(source: &str, text: &str) -> Result<Workflow, String> {
    parse_workflow_with(source, text, &NoActions)
}

/// [`parse_workflow`] plus an [`ActionResolver`] for `uses:` targets
/// that name action directories (`./path`, `owner/repo[/path]@ref`).
pub fn parse_workflow_with(
    source: &str,
    text: &str,
    actions: &dyn ActionResolver,
) -> Result<Workflow, String> {
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
            actions,
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

fn parse_job(
    m: &serde_norway::Mapping,
    id: &str,
    source: &str,
    actions: &dyn ActionResolver,
) -> Result<RawJob, String> {
    let what = format!("{source}.jobs.{id}");
    // Reusable workflows (`jobs.<id>.uses:` + `secrets:`/`with:`/
    // `outputs:` plumbing) are a different feature than step-level
    // `uses:` — name it precisely instead of the generic key error.
    // What's missing for support: a workflow-file resolver
    // (`.github/workflows/*.yml`, local `./` and remote
    // `owner/repo/path@ref` like the action resolver), parsing of
    // the callee's `on.workflow_call` inputs/secrets declarations,
    // a job-graph merge (imported jobs need id namespacing,
    // `needs:` rewriting — the call id fans out to every imported
    // job — `if:` distribution across caller/callee eval contexts,
    // and `outputs:` `jobs.*`→`needs.*` rewriting), and the
    // `secrets:`/`jobs.*` expression contexts the dialect doesn't
    // have at all.
    if get(m, "uses").is_some() {
        return Err(format!(
            "{what}: reusable workflows (`jobs.<id>.uses:` calling another \
             workflow file) are unsupported — they need a workflow-file \
             resolver, `on.workflow_call` input/secret declarations, \
             job-graph expansion with `needs:`/`if:`/`outputs:` rewriting, \
             and `secrets:`/`jobs.*` expression contexts the dialect lacks"
        ));
    }
    if get(m, "secrets").is_some() {
        return Err(format!(
            "{what}: `secrets:` is only meaningful on reusable-workflow calls — unsupported"
        ));
    }
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
        steps.push(parse_step(sm, &sw, &mut step_ids, actions, 0, false)?);
    }
    // `actions/cache` saves in a GitHub post-step — append the
    // job-end flush that drains the pending specs its step wrote.
    if steps.iter().any(|s| {
        matches!(
            s,
            RawStep::Script {
                kind: UsesScript::Cache,
                ..
            }
        )
    }) {
        steps.push(RawStep::Script {
            kind: UsesScript::CacheFlush,
            id: None,
            cond: None,
            timeout_seconds: None,
            with: Vec::new(),
            env: Vec::new(),
        });
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

/// Parse one step mapping — shared by workflow `steps:` lists and
/// composite actions' `runs.steps:` (`in_action` drops
/// `timeout-minutes`, which GitHub doesn't support there). `ids`
/// collects declared step ids for the duplicate check; `depth` is
/// the composite-nesting level (0 = workflow).
fn parse_step(
    sm: &serde_norway::Mapping,
    what: &str,
    ids: &mut HashSet<String>,
    actions: &dyn ActionResolver,
    depth: usize,
    in_action: bool,
) -> Result<RawStep, String> {
    // `id:` is the steps.<id>.outputs.* handle — it becomes a
    // path component in the renderer's output store, so it gets
    // the same charset rule as job output names. Duplicate ids
    // are ambiguous, like on GitHub.
    let id = get(sm, "id")
        .map(|s| scalar(s, &format!("{what}.id")))
        .transpose()?;
    if let Some(id) = &id {
        if !safe_key(id) {
            return Err(format!(
                "{what}.id: invalid step id `{id}` — `[A-Za-z0-9_.-]` only"
            ));
        }
        if !ids.insert(id.clone()) {
            return Err(format!("{what}.id: duplicate step id `{id}`"));
        }
    }
    let (allowed, unsupported): (&[&str], &[&str]) = if in_action {
        (
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
            ],
            &["continue-on-error", "timeout-minutes"],
        )
    } else {
        (
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
        )
    };
    check_keys(sm, allowed, unsupported, what)?;
    let cond = get(sm, "if")
        .map(|v| scalar(v, &format!("{what}.if")))
        .transpose()?;
    let timeout_seconds = get(sm, "timeout-minutes")
        .map(|v| timeout_minutes(v, &format!("{what}.timeout-minutes")))
        .transpose()?;
    let run = get(sm, "run");
    let uses = get(sm, "uses");
    match (run, uses) {
        (Some(_), Some(_)) => Err(format!("{what}: step cannot have both `run` and `uses`")),
        (None, None) => Err(format!("{what}: step needs `run` or `uses`")),
        (Some(rv), None) => {
            if get(sm, "with").is_some() {
                return Err(format!("{what}: `with` requires `uses`"));
            }
            let Value::String(script) = rv else {
                return Err(format!("{what}.run: expected a string"));
            };
            let env = match get(sm, "env") {
                Some(Value::Mapping(em)) => env_map(em, &format!("{what}.env"))?,
                Some(v) => return Err(format!("{what}.env: expected a mapping, got {}", kind(v))),
                None => Vec::new(),
            };
            Ok(RawStep::Run {
                id,
                cond,
                timeout_seconds,
                shell: get(sm, "shell")
                    .map(|s| scalar(s, &format!("{what}.shell")))
                    .transpose()?,
                working_directory: get(sm, "working-directory")
                    .map(|s| scalar(s, &format!("{what}.working-directory")))
                    .transpose()?,
                env,
                script: script.clone(),
            })
        }
        (None, Some(uv)) => {
            let uses = scalar(uv, &format!("{what}.uses"))?;
            // `env:` on a `uses:` step scopes to the action — for
            // script-lowered and composite targets it rides into the
            // synthesized steps; checkout is a verification no-op so
            // env can't have an observable effect there.
            let env = match get(sm, "env") {
                Some(Value::Mapping(em)) => env_map(em, &format!("{what}.env"))?,
                Some(v) => return Err(format!("{what}.env: expected a mapping, got {}", kind(v))),
                None => Vec::new(),
            };
            match parse_uses(&uses, sm, what, actions, depth)? {
                UsesKind::Checkout => Ok(RawStep::Checkout { id, cond }),
                UsesKind::Script(kind, with) => Ok(RawStep::Script {
                    kind,
                    id,
                    cond,
                    timeout_seconds,
                    with,
                    env,
                }),
                UsesKind::Action(action, with) => Ok(RawStep::Composite {
                    id,
                    cond,
                    // `timeout-minutes` becomes a deadline spanning
                    // the inlined steps (see build_composite).
                    timeout_seconds,
                    with,
                    env,
                    action,
                }),
            }
        }
    }
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
#[derive(Clone)]
enum UsesScript {
    /// `dtolnay/rust-toolchain` → rustup install/default.
    RustToolchain,
    /// `actions/upload-artifact` → per-run tar store.
    UploadArtifact,
    /// `actions/download-artifact` → restore from the per-run store.
    DownloadArtifact,
    /// `actions/setup-node` → nodejs.org tarball + toolcache PATH.
    SetupNode,
    /// `actions/setup-python` → python-versions tarball + toolcache.
    SetupPython,
    /// `actions/setup-go` → go.dev tarball + toolcache.
    SetupGo,
    /// `actions/setup-java` → Adoptium API tarball + toolcache
    /// (temurin only).
    SetupJava,
    /// `actions/setup-dotnet` → dot.net dotnet-install.sh + toolcache.
    SetupDotnet,
    /// `actions/cache/restore` → persistent keyed store lookup.
    CacheRestore,
    /// `actions/cache/save` → persistent keyed store write.
    CacheSave,
    /// `actions/cache` → restore now + pending save at job end.
    Cache,
    /// Runner-appended job-end drain of pending cache saves.
    CacheFlush,
    /// `uses: docker://<image>` — a container step lowered to
    /// `docker run` (needs the docker CLI on the runner host).
    /// Carries the image ref (tag/digest included).
    Docker(String),
}

enum UsesKind {
    Checkout,
    /// Script-lowered `uses:` + `with:` pairs normalized to value
    /// lists.
    Script(UsesScript, Vec<(String, Vec<String>)>),
    /// Composite action resolved through the [`ActionResolver`] +
    /// `with:` pairs normalized the same way.
    Action(ActionDef, Vec<(String, Vec<String>)>),
}

/// What a `uses:` target lowers to before `with:` normalization.
enum UsesTarget {
    Checkout,
    Script(UsesScript),
    Action(ActionDef),
}

/// `uses:` — `actions/checkout` is a verification no-op (the
/// worktree is already at the pushed sha); `dtolnay/rust-toolchain`,
/// the `actions/setup-{node,python,go,java,dotnet}` toolchain
/// actions, `actions/{upload,download}-artifact`, the
/// `actions/cache*` family and `docker://<image>` container steps
/// lower to synthesized bash steps; `./local/path` and
/// `owner/repo[/path]@ref` resolve through the [`ActionResolver`] to
/// a composite `action.yml`. Everything else — JS (`runs.using:
/// node*`) and `docker` action *definitions*, unknown actions and
/// reusable-workflow `uses:` — is an explicit error.
fn parse_uses(
    uses: &str,
    sm: &serde_norway::Mapping,
    what: &str,
    actions: &dyn ActionResolver,
    depth: usize,
) -> Result<UsesKind, String> {
    let unsupported = || {
        format!(
            "{what}: unsupported `uses: {uses}` — v1 supports \
             `actions/checkout`, `dtolnay/rust-toolchain`, \
             `actions/setup-node`, `actions/setup-python`, \
             `actions/setup-go`, `actions/setup-java`, \
             `actions/setup-dotnet`, `actions/upload-artifact`, \
             `actions/download-artifact`, `actions/cache`, \
             `actions/cache/save`, `actions/cache/restore`, \
             `docker://<image>` steps, and composite actions via \
             `./path` or `owner/repo[/path]@ref` (JS/docker action \
             definitions, unknown actions and reusable workflows \
             are unsupported)"
        )
    };
    // Local actions: `./path` relative to the repo root — the whole
    // string is the path (GitHub allows no `@ref` there).
    if uses.starts_with("./") {
        return finish_uses(
            UsesTarget::Action(resolve_action(uses, what, actions, depth)?),
            sm,
            what,
        );
    }
    // `docker://` steps: the image spec is the whole `uses:` tail —
    // a `@sha256:` digest must not be mistaken for a git ref, so
    // this check runs before the `@` split below.
    if let Some(image) = uses.strip_prefix("docker://") {
        return finish_uses(
            UsesTarget::Script(UsesScript::Docker(image.to_string())),
            sm,
            what,
        );
    }
    let body = uses.split('@').next().unwrap_or(uses);
    if body.starts_with('/') {
        return Err(format!(
            "{what}: `uses: {uses}` — absolute-path actions are unsupported; \
             use `./` relative to the repo root"
        ));
    }
    let target = if body.eq_ignore_ascii_case("actions/checkout") {
        UsesTarget::Checkout
    } else if body.eq_ignore_ascii_case("dtolnay/rust-toolchain") {
        UsesTarget::Script(UsesScript::RustToolchain)
    } else if body.eq_ignore_ascii_case("actions/upload-artifact") {
        UsesTarget::Script(UsesScript::UploadArtifact)
    } else if body.eq_ignore_ascii_case("actions/download-artifact") {
        UsesTarget::Script(UsesScript::DownloadArtifact)
    } else if body.eq_ignore_ascii_case("actions/setup-node") {
        UsesTarget::Script(UsesScript::SetupNode)
    } else if body.eq_ignore_ascii_case("actions/setup-python") {
        UsesTarget::Script(UsesScript::SetupPython)
    } else if body.eq_ignore_ascii_case("actions/setup-go") {
        UsesTarget::Script(UsesScript::SetupGo)
    } else if body.eq_ignore_ascii_case("actions/setup-java") {
        UsesTarget::Script(UsesScript::SetupJava)
    } else if body.eq_ignore_ascii_case("actions/setup-dotnet") {
        UsesTarget::Script(UsesScript::SetupDotnet)
    } else if body.eq_ignore_ascii_case("actions/cache/restore") {
        UsesTarget::Script(UsesScript::CacheRestore)
    } else if body.eq_ignore_ascii_case("actions/cache/save") {
        UsesTarget::Script(UsesScript::CacheSave)
    } else if body.eq_ignore_ascii_case("actions/cache") {
        UsesTarget::Script(UsesScript::Cache)
    } else if is_remote_action(body) {
        UsesTarget::Action(resolve_action(uses, what, actions, depth)?)
    } else {
        return Err(unsupported());
    };
    finish_uses(target, sm, what)
}

/// `owner/repo[/sub/path]` shape — the remote-action `uses:` form.
/// Segment names are restricted so a malformed ref errors here
/// instead of inside the resolver's fetch.
fn is_remote_action(body: &str) -> bool {
    let seg_ok = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    };
    let mut segs = body.split('/');
    match (segs.next(), segs.next()) {
        (Some(owner), Some(repo)) => {
            // GitHub owners are alnum+hyphen; repos allow `.`/`_`.
            let owner_ok =
                !owner.is_empty() && owner.chars().all(|c| c.is_ascii_alphanumeric() || c == '-');
            seg_ok(repo) && owner_ok && segs.all(|s| seg_ok(s) && !s.contains("..") && s != ".")
        }
        _ => false,
    }
}

/// Fetch + parse the `action.yml` behind a `uses:` target. `depth`
/// is the containing action's nesting level — the cap keeps cyclic
/// action repos from recursing forever.
fn resolve_action(
    uses: &str,
    what: &str,
    actions: &dyn ActionResolver,
    depth: usize,
) -> Result<ActionDef, String> {
    if depth >= MAX_ACTION_DEPTH {
        return Err(format!(
            "{what}: `uses: {uses}` — composite actions nested deeper than \
             {MAX_ACTION_DEPTH} levels are unsupported"
        ));
    }
    let text = actions
        .action_yaml(uses)
        .map_err(|e| format!("{what}: cannot resolve `uses: {uses}` — {e}"))?;
    parse_action(uses, &text, actions, depth, what)
}

/// Shared tail of [`parse_uses`]: `actions/checkout`'s `with.path`
/// check, then `with:` normalization to value lists for the
/// script-lowered and composite kinds.
fn finish_uses(
    target: UsesTarget,
    sm: &serde_norway::Mapping,
    what: &str,
) -> Result<UsesKind, String> {
    let with = match get(sm, "with") {
        Some(Value::Mapping(w)) => Some(w),
        Some(_) => return Err(format!("{what}.with: expected a mapping")),
        None => None,
    };
    if matches!(target, UsesTarget::Checkout) {
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
    }
    // Normalize `with:` values to lists (scalar → one entry,
    // sequence of scalars → many) — shared by script-lowered and
    // composite actions.
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
    match target {
        UsesTarget::Script(kind) => Ok(UsesKind::Script(kind, out)),
        UsesTarget::Action(def) => Ok(UsesKind::Action(def, out)),
        // Handled by the early return above — `with:` on checkout
        // was already vetted there.
        UsesTarget::Checkout => Ok(UsesKind::Checkout),
    }
}

/// Parse a composite action's `action.yml`. Only
/// `runs.using: composite` is supported — JS (`node*`) and `docker`
/// actions are explicit errors. `inputs`/`outputs`/`runs.steps`
/// follow the workflow subset (`runs.steps` entries are ordinary
/// steps minus `timeout-minutes`; nested `uses:` recurse through
/// the same resolver).
fn parse_action(
    uses: &str,
    text: &str,
    actions: &dyn ActionResolver,
    depth: usize,
    what: &str,
) -> Result<ActionDef, String> {
    let awhat = format!("{what} (action `{uses}`)");
    let v: Value = serde_norway::from_str(text)
        .map_err(|e| format!("{awhat}: action.yml parse error: {e}"))?;
    let top = as_map(&v, &awhat)?;
    check_keys(
        top,
        &[
            "name",
            "description",
            "author",
            "branding",
            "inputs",
            "outputs",
            "runs",
        ],
        &[],
        &awhat,
    )?;
    let runs_v = get(top, "runs").ok_or_else(|| format!("{awhat}: missing `runs`"))?;
    let runs = as_map(runs_v, &format!("{awhat}.runs"))?;
    let using = get(runs, "using")
        .map(|u| scalar(u, &format!("{awhat}.runs.using")))
        .transpose()?
        .unwrap_or_default();
    if using != "composite" {
        return Err(format!(
            "{awhat}: `runs.using: {using}` — only composite actions are \
             supported. JS `node*` actions need a Node.js runtime plus \
             the action's bundled `@actions/*` packages; `docker` action \
             definitions need image build/pull plus entrypoint plumbing \
             (distinct from `uses: docker://<image>` *steps*, which are \
             supported) — neither exists in the local-runner model"
        ));
    }
    check_keys(
        runs,
        &["using", "steps"],
        &[
            "main",
            "pre",
            "post",
            "pre-entrypoint",
            "post-entrypoint",
            "image",
            "args",
            "entrypoint",
        ],
        &format!("{awhat}.runs"),
    )?;
    let inputs = match get(top, "inputs") {
        Some(Value::Mapping(im)) => {
            let mut out = Vec::with_capacity(im.len());
            for (k, iv) in im {
                let name = k
                    .as_str()
                    .ok_or_else(|| format!("{awhat}.inputs: names must be strings"))?;
                if !safe_key(name) {
                    return Err(format!(
                        "{awhat}.inputs: invalid input name `{name}` — `[A-Za-z0-9_.-]` only"
                    ));
                }
                let mut spec = ActionInput {
                    required: false,
                    default: None,
                };
                match iv {
                    Value::Null => {}
                    Value::Mapping(mm) => {
                        check_keys(
                            mm,
                            &["description", "required", "default", "type", "options"],
                            &[],
                            &format!("{awhat}.inputs.{name}"),
                        )?;
                        if let Some(rv) = get(mm, "required") {
                            spec.required = match rv {
                                Value::Bool(b) => *b,
                                Value::String(s) => s == "true",
                                _ => {
                                    return Err(format!(
                                        "{awhat}.inputs.{name}.required: expected a bool"
                                    ))
                                }
                            };
                        }
                        if let Some(dv) = get(mm, "default") {
                            spec.default =
                                Some(scalar(dv, &format!("{awhat}.inputs.{name}.default"))?);
                        }
                    }
                    _ => {
                        return Err(format!(
                            "{awhat}.inputs.{name}: expected a mapping or null, got {}",
                            kind(iv)
                        ))
                    }
                }
                out.push((name.to_string(), spec));
            }
            out
        }
        Some(v) => {
            return Err(format!(
                "{awhat}.inputs: expected a mapping, got {}",
                kind(v)
            ))
        }
        None => Vec::new(),
    };
    let outputs = match get(top, "outputs") {
        Some(Value::Mapping(om)) => {
            let mut out = Vec::with_capacity(om.len());
            for (k, v) in om {
                let key = k
                    .as_str()
                    .ok_or_else(|| format!("{awhat}.outputs: names must be strings"))?;
                if !safe_key(key) {
                    return Err(format!(
                        "{awhat}.outputs: invalid output name `{key}` — `[A-Za-z0-9_.-]` only"
                    ));
                }
                out.push((
                    key.to_string(),
                    scalar(v, &format!("{awhat}.outputs.{key}"))?,
                ));
            }
            out
        }
        Some(v) => {
            return Err(format!(
                "{awhat}.outputs: expected a mapping, got {}",
                kind(v)
            ))
        }
        None => Vec::new(),
    };
    let steps_v = get(runs, "steps").ok_or_else(|| format!("{awhat}.runs: missing `steps`"))?;
    let Value::Sequence(seq) = steps_v else {
        return Err(format!("{awhat}.runs.steps: expected a sequence"));
    };
    if seq.is_empty() {
        return Err(format!("{awhat}.runs.steps: empty"));
    }
    let mut steps = Vec::with_capacity(seq.len());
    let mut step_ids: HashSet<String> = HashSet::new();
    for (si, sv) in seq.iter().enumerate() {
        let sw = format!("{awhat}.runs.steps[{si}]");
        let sm = as_map(sv, &sw)?;
        steps.push(parse_step(
            sm,
            &sw,
            &mut step_ids,
            actions,
            depth + 1,
            true,
        )?);
    }
    Ok(ActionDef {
        inputs,
        outputs,
        steps,
    })
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
             matrix.*, inputs.* (inside composite actions), \
             needs.<job>.{{result,outputs.<name>}}, \
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
        "inputs" => {
            if rest.is_empty() {
                return Err(bad());
            }
            let Some(inputs) = eval.inputs else {
                return Err(format!(
                    "{what}: `inputs.*` is only available inside composite actions"
                ));
            };
            Ok(Cond::Lit(
                inputs
                    .iter()
                    .rev()
                    .find(|(k, _)| k == rest)
                    .map(|(_, v)| v.clone())
                    .unwrap_or_default(),
            ))
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
            inputs: None,
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

    /// Parse + expand; returns the error text on failure.
    fn plan_err(text: &str) -> String {
        let wf = parse_workflow("ci.yml", text).unwrap();
        match wf.expand(GhaContext {
            sha: "s",
            git_ref: "refs/heads/main",
            repository: "o/r",
            event_name: "push",
        }) {
            Err(e) => e,
            Ok(_) => panic!("expected a plan error"),
        }
    }

    fn expanded_scripts(text: &str) -> Vec<String> {
        let wf = parse_workflow("ci.yml", text).unwrap();
        wf.expand(GhaContext {
            sha: "s",
            git_ref: "refs/heads/main",
            repository: "o/r",
            event_name: "push",
        })
        .unwrap()
        .into_iter()
        .flat_map(|j| j.steps)
        .filter_map(|s| match s {
            Step::Run(r) => Some(r.script),
            _ => None,
        })
        .collect()
    }

    #[test]
    fn setup_actions_lower_to_toolchain_scripts() {
        let scripts = expanded_scripts(
            r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/setup-node@v4
        with:
          node-version: '22'
      - uses: actions/setup-python@v5
        with:
          python-version: '3.12'
      - uses: actions/setup-go@v5
        with:
          go-version: '1.22'
      - uses: actions/setup-java@v4
        with:
          java-version: '21'
          distribution: temurin
      - uses: actions/setup-dotnet@v4
        with:
          dotnet-version: '8.0.x'
"#,
        );
        assert_eq!(scripts.len(), 5);
        assert!(scripts[0].contains("nodejs.org/dist"), "{}", scripts[0]);
        assert!(scripts[0].contains("toolcache/node"), "{}", scripts[0]);
        assert!(
            scripts[1].contains("actions/python-versions"),
            "{}",
            scripts[1]
        );
        assert!(scripts[2].contains("go.dev/dl"), "{}", scripts[2]);
        assert!(scripts[2].contains("GOROOT"), "{}", scripts[2]);
        assert!(scripts[3].contains("api.adoptium.net"), "{}", scripts[3]);
        assert!(scripts[3].contains("JAVA_HOME"), "{}", scripts[3]);
        assert!(scripts[4].contains("dotnet-install.sh"), "{}", scripts[4]);
        assert!(scripts[4].contains("DOTNET_ROOT"), "{}", scripts[4]);
    }

    #[test]
    fn cache_actions_lower_and_append_flush() {
        // actions/cache is restore-now + job-end save: the job gains
        // a runner-appended flush step, so 3 uses: → 4 run steps.
        let scripts = expanded_scripts(
            r#"
on: push
jobs:
  t:
    steps:
      - uses: actions/cache/restore@v4
        with:
          key: k1
          path: .cache/
      - uses: actions/cache/save@v4
        with:
          key: k2
          path: .cache/
      - uses: actions/cache@v4
        with:
          key: k3
          path: .cache/
          restore-keys: k
"#,
        );
        assert_eq!(scripts.len(), 4, "{scripts:?}");
        assert!(scripts[0].contains("bupha_cache_restore"), "{}", scripts[0]);
        assert!(scripts[1].contains("bupha_cache_save"), "{}", scripts[1]);
        assert!(scripts[2].contains("bupha-cache-pending"), "{}", scripts[2]);
        assert!(scripts[3].contains("bupha-cache-pending"), "{}", scripts[3]);
        assert!(scripts[3].contains("bupha_cache_save"), "{}", scripts[3]);
    }

    #[test]
    fn setup_and_cache_with_key_errors() {
        let cases: &[(&str, &str)] = &[
            // unknown with: keys hard-error per action
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-node@v4\n        with:\n          node-version: '22'\n          bogus: 1\n",
                "bogus",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-python@v5\n        with:\n          python-version: '3.12'\n          bogus: 1\n",
                "bogus",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-go@v5\n        with:\n          go-version: '1.22'\n          bogus: 1\n",
                "bogus",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/cache@v4\n        with:\n          key: k\n          path: p\n          bogus: 1\n",
                "bogus",
            ),
            // version inputs required
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-node@v4\n",
                "node-version",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-python@v5\n",
                "python-version",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-go@v5\n",
                "go-version",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-java@v4\n",
                "java-version",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-java@v4\n        with:\n          java-version: '21'\n          distribution: zulu\n",
                "zulu",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-dotnet@v4\n        with:\n          dotnet-version: '8.0.x'\n          dotnet-quality: signed\n",
                "dotnet-quality",
            ),
            // version-file must stay in the worktree
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-node@v4\n        with:\n          node-version-file: ../x\n",
                "node-version-file",
            ),
            // cache key/path required
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/cache/save@v4\n        with:\n          path: p\n",
                "key",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/cache/restore@v4\n        with:\n          key: k\n",
                "path",
            ),
        ];
        for (i, (wf, needle)) in cases.iter().enumerate() {
            let err = plan_err(wf);
            assert!(err.contains(needle), "case {i}: {err} lacks `{needle}`");
        }
    }

    #[test]
    fn unsupported_uses_still_error() {
        for uses in [
            "actions/setup-haskell@v2",
            "./local/action",
            "octo/repo/.github/actions/x@v1",
            "octo/repo/.github/workflows/ci.yml@v1",
        ] {
            let wf = format!("on: push\njobs:\n  t:\n    steps:\n      - uses: {uses}\n");
            assert!(parse_workflow("ci.yml", &wf).is_err(), "{uses}");
        }
    }

    #[test]
    fn docker_uses_lowers_to_docker_run() {
        let scripts = expanded_scripts(
            r#"
on: push
jobs:
  t:
    steps:
      - uses: docker://alpine:3.20
        with:
          entrypoint: /bin/sh
          args: ['-c', 'echo hi > out.txt']
      - uses: docker://img@sha256:abc123
"#,
        );
        assert_eq!(scripts.len(), 2);
        assert!(scripts[0].contains("docker run"), "{}", scripts[0]);
        assert!(scripts[0].contains("'alpine:3.20'"), "{}", scripts[0]);
        assert!(
            scripts[0].contains("--entrypoint '/bin/sh'"),
            "{}",
            scripts[0]
        );
        assert!(
            scripts[0].contains("'-c' 'echo hi > out.txt'"),
            "{}",
            scripts[0]
        );
        assert!(scripts[0].contains("--env-file"), "{}", scripts[0]);
        // The digest form keeps `@sha256:` whole — not split as a ref.
        assert!(scripts[1].contains("'img@sha256:abc123'"), "{}", scripts[1]);
    }

    #[test]
    fn docker_uses_bad_input_errors() {
        for (wf, needle) in [
            // unknown with: key
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: docker://alpine\n        with:\n          bogus: 1\n",
                "bogus",
            ),
            // multi-value entrypoint
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: docker://alpine\n        with:\n          entrypoint: ['/a', '/b']\n",
                "entrypoint",
            ),
            // empty / flag-like / whitespace image specs
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: docker://\n",
                "invalid docker image",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: docker://-evil\n",
                "invalid docker image",
            ),
            (
                "on: push\njobs:\n  t:\n    steps:\n      - uses: \"docker://a b\"\n",
                "invalid docker image",
            ),
        ] {
            let err = expand_err(wf, &FakeActions::new(&[]));
            assert!(err.contains(needle), "{needle}: {err}");
        }
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

    // --- composite actions (`uses: ./path`, `owner/repo[/path]@ref`) ---

    /// Fake resolver — `uses` → canned action.yml text.
    struct FakeActions(Vec<(String, String)>);

    impl FakeActions {
        fn new(pairs: &[(&str, &str)]) -> Self {
            Self(
                pairs
                    .iter()
                    .map(|(u, y)| (u.to_string(), y.to_string()))
                    .collect(),
            )
        }
    }

    impl ActionResolver for FakeActions {
        fn action_yaml(&self, uses: &str) -> Result<String, String> {
            self.0
                .iter()
                .find(|(u, _)| u == uses)
                .map(|(_, y)| y.clone())
                .ok_or_else(|| format!("no such action `{uses}`"))
        }
    }

    const GREET_ACTION: &str = r#"
name: greet
inputs:
  name:
    required: true
  punct:
    default: "!"
outputs:
  greeting: ${{ steps.gen.outputs.text }}
runs:
  using: composite
  steps:
    - id: gen
      run: echo "text=hi ${{ inputs.name }}${{ inputs.punct }}" >> "$GITHUB_OUTPUT"
"#;

    fn expand_with(text: &str, resolver: &dyn ActionResolver) -> Result<Vec<Job>, String> {
        let wf = parse_workflow_with("ci.yml", text, resolver)?;
        wf.expand(GhaContext {
            sha: "s",
            git_ref: "refs/heads/main",
            repository: "o/r",
            event_name: "push",
        })
    }

    /// Expansion error text — `Job` has no `Debug`, so `unwrap_err`
    /// can't be used on the `Ok` side.
    fn expand_err(text: &str, resolver: &dyn ActionResolver) -> String {
        match expand_with(text, resolver) {
            Err(e) => e,
            Ok(_) => panic!("expected an expansion error"),
        }
    }

    #[test]
    fn composite_local_action_inlines_steps_and_outputs() {
        let resolver = FakeActions::new(&[("./.github/actions/greet", GREET_ACTION)]);
        let jobs = expand_with(
            r#"
on: push
jobs:
  t:
    steps:
      - id: greet
        uses: ./.github/actions/greet
        with:
          name: world
"#,
            &resolver,
        )
        .unwrap();
        // inner run step + the Publish step for the uses-id.
        assert_eq!(jobs[0].steps.len(), 2);
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected inner run step");
        };
        assert_eq!(r.id.as_deref(), Some("gen"));
        assert!(
            r.script.contains("hi world!"),
            "input not interpolated: {}",
            r.script
        );
        let Step::Publish { id, outputs, .. } = &jobs[0].steps[1] else {
            panic!("expected publish step");
        };
        assert_eq!(id, "greet");
        // The output template's steps.* ref stays deferred.
        assert_eq!(
            outputs,
            &vec![(
                "greeting".to_string(),
                "${{ steps.gen.outputs.text }}".to_string()
            )]
        );
    }

    #[test]
    fn composite_remote_action_resolves_same_way() {
        let resolver = FakeActions::new(&[("octo/actions/greet@v1", GREET_ACTION)]);
        let jobs = expand_with(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: octo/actions/greet@v1\n        with:\n          name: octo\n",
            &resolver,
        )
        .unwrap();
        // No uses-step id → no publish step; just the inner run.
        assert_eq!(jobs[0].steps.len(), 1);
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert!(r.script.contains("hi octo!"), "{}", r.script);
    }

    #[test]
    fn composite_inputs_contract_is_enforced() {
        let resolver = FakeActions::new(&[("./a", GREET_ACTION)]);
        // required input missing
        let err = expand_err(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./a\n",
            &resolver,
        );
        assert!(err.contains("name"), "{err}");
        // undeclared with: key
        let err = expand_err(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./a\n        with:\n          name: x\n          bogus: 1\n",
            &resolver,
        );
        assert!(err.contains("bogus"), "{err}");
        // default fills in when not provided
        let jobs = expand_with(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./a\n        with:\n          name: bob\n",
            &resolver,
        )
        .unwrap();
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert!(r.script.contains("bob!"), "{}", r.script);
    }

    #[test]
    fn composite_nested_actions_and_uses_if_gate() {
        let inner = r#"
inputs:
  v:
    required: true
runs:
  using: composite
  steps:
    - run: echo "inner ${{ inputs.v }}" > inner.txt
"#;
        let outer = r#"
inputs:
  v:
    default: dflt
runs:
  using: composite
  steps:
    - uses: ./inner
      with:
        v: ${{ inputs.v }}-x
"#;
        let resolver = FakeActions::new(&[("./outer", outer), ("./inner", inner)]);
        let jobs = expand_with(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./outer\n        if: github.ref_name == 'main'\n",
            &resolver,
        )
        .unwrap();
        // outer uses has no id → inner run step only, gated by the
        // distributed uses-step `if:` (ANDed with implicit success()).
        assert_eq!(jobs[0].steps.len(), 1);
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert!(r.script.contains("inner dflt-x"), "{}", r.script);
        let dbg = format!("{:?}", r.cond);
        assert!(dbg.contains("Lit(\"main\")"), "{dbg}");
    }

    #[test]
    fn composite_unsupported_shapes_error() {
        let resolver = FakeActions::new(&[
            ("./js", "runs:\n  using: node20\n  main: index.js\n"),
            ("./docker", "runs:\n  using: docker\n  image: alpine\n"),
        ]);
        for uses in ["./js", "./docker"] {
            let wf = format!("on: push\njobs:\n  t:\n    steps:\n      - uses: {uses}\n");
            let err = expand_err(&wf, &resolver);
            assert!(err.contains("composite"), "{uses}: {err}");
        }
        // missing action.yml → resolver error surfaces
        let err = expand_err(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./missing\n",
            &resolver,
        );
        assert!(err.contains("cannot resolve"), "{err}");
    }

    #[test]
    fn composite_timeout_minute_tags_step_deadlines() {
        let resolver = FakeActions::new(&[("./ok", GREET_ACTION)]);
        let jobs = expand_with(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./ok\n        with:\n          name: x\n        timeout-minutes: 2.5\n",
            &resolver,
        )
        .unwrap();
        // inner run step + publish step; only the run step carries
        // the deadline (150s = 2.5min).
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert_eq!(
            r.deadlines,
            vec![("jobs.t.steps[0]".to_string(), 150)],
            "{:?}",
            r.deadlines
        );
        // A composite without timeout-minutes tags nothing.
        let jobs = expand_with(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./ok\n        with:\n          name: x\n",
            &resolver,
        )
        .unwrap();
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert!(r.deadlines.is_empty());
    }

    #[test]
    fn inputs_context_outside_composite_errors() {
        let err = expand_err(
            "on: push\njobs:\n  t:\n    steps:\n      - run: echo ${{ inputs.x }}\n",
            &FakeActions::new(&[]),
        );
        assert!(err.contains("inputs"), "{err}");
    }

    #[test]
    fn uses_step_env_scopes_to_action_steps() {
        // Script-lowered uses: `env:` lands on the synthesized
        // RunStep (previously it was silently dropped).
        let wf = parse_workflow(
            "ci.yml",
            "on: push\njobs:\n  t:\n    steps:\n      - uses: actions/setup-node@v4\n        with:\n          node-version: '22'\n        env:\n          NODE_ENV: test\n",
        )
        .unwrap();
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
        assert!(
            r.env
                .contains(&("NODE_ENV".to_string(), "test".to_string())),
            "{:?}",
            r.env
        );

        // Composite uses: env applies to every inlined step, under
        // each step's own env (step env wins on conflicts).
        let action = "runs:\n  using: composite\n  steps:\n    - run: echo x\n      env:\n        PUNCT: inner\n";
        let resolver = FakeActions::new(&[("./a", action)]);
        let jobs = expand_with(
            "on: push\njobs:\n  t:\n    steps:\n      - uses: ./a\n        env:\n          GREETING: hi\n          PUNCT: outer\n",
            &resolver,
        )
        .unwrap();
        let Step::Run(r) = &jobs[0].steps[0] else {
            panic!("expected run step");
        };
        assert!(
            r.env.contains(&("GREETING".to_string(), "hi".to_string())),
            "{:?}",
            r.env
        );
        assert!(
            r.env.contains(&("PUNCT".to_string(), "inner".to_string())),
            "{:?}",
            r.env
        );
    }

    #[test]
    fn reusable_workflow_uses_errors_precisely() {
        let err = parse_workflow(
            "ci.yml",
            "on: push\njobs:\n  call:\n    uses: octo/repo/.github/workflows/ci.yml@v1\n",
        )
        .err()
        .unwrap();
        assert!(err.contains("reusable workflows"), "{err}");
        let err = parse_workflow(
            "ci.yml",
            "on: push\njobs:\n  call:\n    secrets: inherit\n    steps:\n      - run: echo hi\n",
        )
        .err()
        .unwrap();
        assert!(err.contains("secrets"), "{err}");
    }
}
