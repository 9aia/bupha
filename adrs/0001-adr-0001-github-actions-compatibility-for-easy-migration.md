+++
id = "ADR-0001"
title = "ADR-0001: GitHub Actions compatibility for easy migration"
status = "done"
priority = "medium"
+++

# ADR-0001: GitHub Actions compatibility for easy migration

Status: accepted

## Context

The single biggest adoption barrier is that every prospective user
already has `.github/workflows/*.yml`. Asking them to rewrite pipelines
as `ci.sh`/dagger before they can try bupha kills the evaluation.

## Options

**A. One-shot converter — `bupha migrate`** (recommended)
Parse a GHA workflow YAML and emit an equivalent `ci.sh` +
`bupha.toml` skeleton: `on.push` → webhook wiring, `runs-on` → runner
choice, `steps.run` blocks → sequential shell lines, `uses:` actions →
annotated TODO comments for manual porting. Deterministic, offline,
no runtime cost, output is *owned by the user* afterwards — no ongoing
YAML interpreter to maintain.

**B. `gha` runner kind — interpret workflow YAML at runtime**
A new `Runner` impl that executes a workflow file per push. Maximum
fidelity (matrix, expressions, `uses:`) but pulls the entire GHA
expression language + action resolution into bupha — a second product
inside the product. Rejected for scope.

**C. Hybrid: convert + vendored `act`**
Shell out to `nektos/act` for the migration period. Heavy dep, mixed
semantics (act approximates, doesn't match), confusing blame when a
run fails. Rejected.

## Proposal

Ship `bupha migrate [--workflow .github/workflows/ci.yml]`:

1. Parse `on.push.branches` → `[[repo]]` `branch`
2. Each `run` step → a line in generated `ci.sh` (env/working-directory
   preserved)
3. `uses:` steps → `# TODO: port <action>@<ver>` comments (checkout →
   implicit, cache → `[cache] conf`)
4. Matrix → emit one `ci.sh` variant per axis combo or a comment
   block listing the axes for manual fan-out
5. Print a coverage report: N steps converted, M actions need porting

Non-goals: GHA expression syntax (`${{ }}` beyond `env`/simple vars),
reusable workflows, `uses:` action compatibility, matrix fan-out.

## Consequences

- Migration becomes a 30-second exercise instead of a rewrite
- The canonical pipeline stays `ci.sh`/dagger — no YAML interpreter
  debt in the core
- Partial conversions are honest (TODO comments, coverage report)
  rather than silently wrong
