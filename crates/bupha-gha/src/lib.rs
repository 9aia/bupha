//! bupha-gha — runner impl that executes `.github/workflows/*.yml`
//! natively: GitHub Actions as bupha's first CI dialect. The pushed
//! sha's detached worktree is the workspace; workflows are parsed by
//! the pure `plan` core, rendered to bash by `script`, and spawned
//! as one child the watcher waits on.
//!
//! The supported subset and its hard boundaries live in the crate
//! README — anything outside it is a loud error at spawn time, never
//! a silent skip.

/// The pure workflow core — public so a sibling dialect (gitlab)
/// or tooling can reuse parse/select/expand without the runner.
pub mod plan;
mod script;

use bupha_runner::{RunCtx, RunHandle, Runner};
use plan::{Event, GhaContext, Job, Workflow};
use serde::Deserialize;
use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

/// gha runner config — impl-owned fields from `runner.extra`.
#[derive(Debug, Deserialize)]
pub struct GhaCfg {
    /// Pin workflow file(s) instead of discovering every
    /// `on.push`-matching file under `.github/workflows/`. Entries
    /// resolve against `.github/workflows/` first, then the
    /// worktree root.
    #[serde(default)]
    pub workflow: Option<WorkflowSel>,
}

/// `runner.workflow = "ci.yml"` or a list of them.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub enum WorkflowSel {
    One(String),
    Many(Vec<String>),
}

impl WorkflowSel {
    fn entries(&self) -> &[String] {
        match self {
            Self::One(s) => std::slice::from_ref(s),
            Self::Many(v) => v,
        }
    }
}

pub struct GhaRunner {
    cfg: GhaCfg,
}

impl GhaRunner {
    pub fn new(cfg: GhaCfg) -> Self {
        Self { cfg }
    }

    /// Workflow files to consider, sorted for determinism.
    fn workflow_files(&self, workdir: &Path) -> io::Result<Vec<PathBuf>> {
        let wfdir = workdir.join(".github/workflows");
        match &self.cfg.workflow {
            Some(sel) => {
                let mut out = Vec::new();
                for e in sel.entries() {
                    let p = wfdir.join(e);
                    let p = if p.is_file() {
                        p
                    } else {
                        let alt = workdir.join(e);
                        if alt.is_file() {
                            alt
                        } else {
                            return Err(io::Error::other(format!(
                                "gha: workflow `{e}` not found under {} or {}",
                                wfdir.display(),
                                workdir.display()
                            )));
                        }
                    };
                    out.push(p);
                }
                Ok(out)
            }
            None => {
                let rd = std::fs::read_dir(&wfdir).map_err(|e| {
                    io::Error::other(format!(
                        "gha: cannot read {}: {e} — no .github/workflows?",
                        wfdir.display()
                    ))
                })?;
                let mut out: Vec<PathBuf> = rd
                    .filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.extension()
                            .and_then(|x| x.to_str())
                            .map(|x| x == "yml" || x == "yaml")
                            .unwrap_or(false)
                    })
                    .collect();
                out.sort();
                if out.is_empty() {
                    return Err(io::Error::other(format!(
                        "gha: no *.yml/*.yaml workflows in {}",
                        wfdir.display()
                    )));
                }
                Ok(out)
            }
        }
    }
}

/// Files changed by the push — v1 diffs the tip commit against its
/// first parent (`HEAD^`), falling back to the whole tree on a root
/// commit. Multi-commit pushes undercount; a `before` sha in the
/// queue is the follow-up (see README).
fn changed_files(workdir: &Path) -> io::Result<Vec<String>> {
    let diff = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["diff", "--name-only", "HEAD^", "HEAD"])
        .output()?;
    if diff.status.success() {
        return Ok(String::from_utf8_lossy(&diff.stdout)
            .lines()
            .map(str::to_string)
            .filter(|l| !l.is_empty())
            .collect());
    }
    let ls = Command::new("git")
        .arg("-C")
        .arg(workdir)
        .args(["ls-files"])
        .output()?;
    if !ls.status.success() {
        return Err(io::Error::other(
            "gha: cannot list changed files (git diff HEAD^ HEAD and git ls-files both failed)",
        ));
    }
    Ok(String::from_utf8_lossy(&ls.stdout)
        .lines()
        .map(str::to_string)
        .filter(|l| !l.is_empty())
        .collect())
}

fn repository_of(key: &str) -> &str {
    key.split(':').next().unwrap_or(key)
}

impl Runner for GhaRunner {
    fn name(&self) -> &'static str {
        "gha"
    }

    fn spawn(&self, ctx: &RunCtx) -> io::Result<Box<dyn RunHandle>> {
        let files = self.workflow_files(ctx.workdir)?;
        let changed = changed_files(ctx.workdir)?;
        let ev = Event {
            git_ref: ctx.git_ref,
            changed_files: &changed,
        };
        let gha_ctx = GhaContext {
            sha: ctx.sha,
            git_ref: ctx.git_ref,
            repository: repository_of(ctx.key),
            event_name: "push",
        };

        // Parse every candidate up front — a broken workflow is a
        // loud spawn failure even if its filters wouldn't match.
        let mut workflows: Vec<Workflow> = Vec::new();
        for f in &files {
            let text = std::fs::read_to_string(f)
                .map_err(|e| io::Error::other(format!("gha: read {}: {e}", f.display())))?;
            let source = f
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("workflow")
                .to_string();
            let wf = plan::parse_workflow(&source, &text)
                .map_err(|e| io::Error::other(format!("gha: {e}")))?;
            if wf.matches_push(&ev).map_err(io::Error::other)? {
                workflows.push(wf);
            }
        }

        // Expand + topo-order per workflow; `needs` never cross
        // files, so each workflow's indices offset by the jobs
        // already collected.
        let mut jobs: Vec<Job> = Vec::new();
        for wf in &workflows {
            let mut expanded = wf
                .expand(gha_ctx)
                .map_err(|e| io::Error::other(format!("gha: {}: {e}", wf.name)))?;
            let base = jobs.len();
            for j in &mut expanded {
                for n in &mut j.needs {
                    *n += base;
                }
            }
            jobs.append(&mut expanded);
        }

        let script = if jobs.is_empty() {
            format!(
                "#!/usr/bin/env bash\necho {}\n",
                script::sq(&format!(
                    "gha: no on.push workflow matched ref {}",
                    ctx.git_ref
                ))
            )
        } else {
            script::render(
                &jobs,
                &script::ScriptCtx {
                    workdir: &ctx.workdir.to_string_lossy(),
                    sha: ctx.sha,
                    git_ref: ctx.git_ref,
                    repository: gha_ctx.repository,
                },
            )
            .map_err(io::Error::other)?
        };

        let mut c = Command::new("bash");
        c.current_dir(ctx.workdir).arg("-c").arg(&script);
        Ok(Box::new(c.spawn()?))
    }
}
