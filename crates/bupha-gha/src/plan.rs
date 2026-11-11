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
    needs: Vec<String>,
    runs_on: Vec<String>,
    env: Vec<(String, String)>,
    defaults: Defaults,
    matrix: Vec<(String, Vec<String>)>,
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
        shell: Option<String>,
        working_directory: Option<String>,
        env: Vec<(String, String)>,
        script: String,
    },
    /// `uses: actions/checkout` — verification no-op (the worktree
    /// is already at the pushed sha).
    Checkout { id: Option<String> },
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
    pub steps: Vec<Step>,
}

pub enum Step {
    Run(RunStep),
    Checkout { id: Option<String> },
}

pub struct RunStep {
    /// `id:` — `steps.<id>.outputs.*` handle; `None` when the step
    /// declared no id (its `$GITHUB_OUTPUT` writes stay unreferenced).
    pub id: Option<String>,
    pub shell: Option<String>,
    pub working_directory: Option<String>,
    /// Step-level env only (job env is exported by the job block).
    pub env: Vec<(String, String)>,
    pub script: String,
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
        RawStep::Checkout { id } => Ok(Step::Checkout { id: id.clone() }),
        RawStep::Run {
            id,
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

/// Is `inner` a `needs.<job>.outputs.<name>` or
/// `steps.<id>.outputs.<name>` ref — the only expressions deferred
/// to runtime? The split is at the FIRST `.outputs.`: the middle is
/// the job/step id, everything after is the output name (dots
/// allowed).
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
            // Output refs can't resolve at expand time — the values
            // are produced while the generated program runs. Keep the
            // `${{ … }}` source verbatim; the renderer turns it into
            // a lookup against the collected $GITHUB_OUTPUT files.
            // Everything else under these contexts stays unsupported.
            if is_output_ref(inner) {
                Ok(format!("${{{{ {inner} }}}}"))
            } else {
                Err(format!(
                    "{what}: unsupported expression `${{{{ {inner} }}}}` — \
                     under `needs`/`steps` only `.outputs.<name>` refs are \
                     supported (needs.*.result, steps.*.conclusion, … are not)"
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
            "needs",
            "runs-on",
            "env",
            "defaults",
            "steps",
            "strategy",
            "permissions",
            "outputs",
        ],
        // The v1 boundary, loudly: conditions, containers,
        // services, timeouts, concurrency.
        &[
            "if",
            "timeout-minutes",
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
    let matrix = parse_matrix(get(m, "strategy"), &what)?;
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
                "run",
                "uses",
                "with",
                "working-directory",
                "env",
                "shell",
            ],
            &["if", "timeout-minutes", "continue-on-error"],
            &sw,
        )?;
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
                parse_uses(&uses, sm, &sw)?;
                steps.push(RawStep::Checkout { id });
            }
        }
    }
    Ok(RawJob {
        id: id.to_string(),
        name,
        needs,
        runs_on,
        env,
        defaults,
        matrix,
        outputs,
        steps,
    })
}

/// `uses:` — only `actions/checkout` exists in v1 (a verification
/// no-op: the worktree is already at the pushed sha). Everything
/// else, `actions/setup-*` included, is an explicit error.
fn parse_uses(uses: &str, sm: &serde_norway::Mapping, what: &str) -> Result<(), String> {
    let unsupported = || {
        format!(
            "{what}: unsupported `uses: {uses}` — v1 supports only \
             `actions/checkout` (`actions/setup-*` is a later milestone)"
        )
    };
    let body = uses.split('@').next().unwrap_or(uses);
    if body.starts_with("docker://") || body.starts_with("./") || body.starts_with('/') {
        return Err(unsupported());
    }
    if !body.eq_ignore_ascii_case("actions/checkout") {
        return Err(unsupported());
    }
    match get(sm, "with") {
        Some(Value::Mapping(w)) => {
            if get(w, "path").is_some() {
                return Err(format!(
                    "{what}: `with.path` on actions/checkout is unsupported — \
                     the worktree root is fixed"
                ));
            }
            // Other checkout inputs (ref, fetch-depth, token, …)
            // don't change local semantics: the sha is checked out.
        }
        Some(_) => return Err(format!("{what}.with: expected a mapping")),
        None => {}
    }
    Ok(())
}

fn parse_matrix(v: Option<&Value>, what: &str) -> Result<Vec<(String, Vec<String>)>, String> {
    let Some(v) = v else { return Ok(Vec::new()) };
    let m = as_map(v, &format!("{what}.strategy"))?;
    check_keys(
        m,
        &["matrix"],
        &["fail-fast", "max-parallel"],
        &format!("{what}.strategy"),
    )?;
    let Some(mv) = get(m, "matrix") else {
        return Ok(Vec::new());
    };
    let mm = as_map(mv, &format!("{what}.strategy.matrix"))?;
    let mut out = Vec::with_capacity(mm.len());
    for (k, vv) in mm {
        let key = k
            .as_str()
            .ok_or_else(|| format!("{what}.strategy.matrix: keys must be strings"))?;
        let Value::Sequence(seq) = vv else {
            return Err(format!(
                "{what}.strategy.matrix.{key}: expected a list — \
                 `include`/`exclude` are not supported in v1"
            ));
        };
        if seq.is_empty() {
            return Err(format!("{what}.strategy.matrix.{key}: empty list"));
        }
        let vals = seq
            .iter()
            .map(|i| scalar(i, &format!("{what}.strategy.matrix.{key}")))
            .collect::<Result<Vec<_>, _>>()?;
        out.push((key.to_string(), vals));
    }
    Ok(out)
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

    /// Everything else under needs./steps. still errors loudly.
    #[test]
    fn non_output_needs_and_steps_refs_error() {
        let e = eval(&[], None);
        for bad in [
            "needs.build.result",
            "needs.build.outputs",
            "needs..outputs.x",
            "steps.s1.conclusion",
            "steps.s1.outputs",
            "steps..outputs.x",
            "steps.s1.outcome",
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
}
