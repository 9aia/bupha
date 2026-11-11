# Dogfood: real GitHub Actions workflows through `runner.kind = "gha"`

Parity report for the `gha` dialect (org-ledger T-003). Method:
real `.github/workflows/ci.yml` files from public Rust repos —
the 9aia org repos themselves carry **no** GitHub workflows (CI
is hooks + myco/sturn, not GHA), so "real" means upstream public
repos — dropped into a scratch git repo and run through
`bupha run` with `runner.kind = "gha"`. Unsupported constructs
were then peeled iteratively (each `spawn failed:` error fixed,
re-run) until each workflow planned **and executed**, recording
every error along the way.

Workflows tested (fetched 2026-11, upstream `main`/`master`):

| repo | file | jobs | verdict |
| --- | --- | --- | --- |
| dtolnay/anyhow | `ci.yml` (146 lines) | 9 | hard error at spawn; plans+executes after peeling ~35 unsupported constructs |
| serde-rs/serde | `ci.yml` (179 lines) | 10 | hard error at spawn; plans+executes after peeling ~40 unsupported constructs |
| BurntSushi/ripgrep | `ci.yml` (291 lines) | 6 | hard error at spawn; plans+executes after peeling ~20 unsupported constructs |
| synthetic subset port | — | 3 | **full pass** — every supported feature exercised green |

## First-error results (unmodified workflows)

| workflow | first `spawn failed:` error |
| --- | --- |
| anyhow `ci.yml` | `gha: ci.yml.jobs.pre_ci: unsupported key 'uses'` — job-level reusable-workflow call (`uses: dtolnay/.github/.github/workflows/pre_ci.yml@master`, line 17) |
| serde `ci.yml` | `gha: ci.yml.jobs.test: unsupported key 'timeout-minutes'` (line 19) |
| ripgrep `ci.yml` | `gha: ci.yml.jobs.test.env.TARGET_FLAGS: expected a scalar, got null` — **bug, fixed** (see below); next error is `jobs.test.strategy: unsupported key 'fail-fast'` (line 51) |

Parse is all-or-nothing per file and stops at the first error —
the tables below are the *complete* inventory obtained by peeling.

## Unsupported-syntax inventory hit during peeling

Every real workflow hit the same wall, in roughly this order:

| construct | where it appears | gha verdict |
| --- | --- | --- |
| job-level `uses:` (reusable workflows) | anyhow `pre_ci` (L17) | hard error `unsupported key 'uses'` |
| job `if:` | anyhow L22,54,73,85,98,114,126,140; serde L143,157,173; ripgrep step-level mostly | hard error `unsupported key 'if'` |
| `timeout-minutes` | every job in anyhow/serde | hard error `unsupported key 'timeout-minutes'` |
| `strategy.fail-fast` | anyhow L25,57; serde L44,61,85; ripgrep L51 | hard error `unsupported key 'fail-fast'` |
| step `if:` | anyhow L37,40,45; serde L75,79; ripgrep L123,131,143,159,185,193,201,240,258,273,281,293 | hard error `unsupported key 'if'` |
| step `continue-on-error` | anyhow L49; serde L29 | hard error `unsupported key 'continue-on-error'` |
| `uses:` ≠ `actions/checkout` | `dtolnay/rust-toolchain` (everywhere), `dtolnay/install`, `actions/upload-artifact`, `docker/setup-docker-action`, `actions/cache`, `actions/setup-node`, `docker://pandoc/…` | hard error `unsupported 'uses: …'` |
| `strategy.matrix.include` | ripgrep L53–117, mdBook main.yml | `expected a scalar, got mapping` |
| complex `${{ }}` expressions | serde L58 job name (`matrix.os == 'windows' && … \|\| ''`), ripgrep `startsWith(...)`, `hashFiles(...)`, `steps.*.outputs.*` | hard error `unsupported expression` |
| `env:` null values | ripgrep L40–42 (`TARGET_FLAGS:` empty) | **was a bug — fixed** (null now coerces to `""`) |
| step `with:` on any `uses:` | ripgrep L133–135, L249–257 | **was a bug — fixed** (`with` was missing from the step key allowlist; `actions/checkout` `with:` inputs now accepted, `with.path` still errors, `with` on `run:` errors) |
| job `name:` `${{ }}` | serde L41,58,82; anyhow L20,53 | **was a bug — fixed** (names are now interpolated; unsupported expressions in names now error loudly instead of passing through verbatim) |

Triggers `pull_request`, `workflow_dispatch`, `schedule`,
`merge_group` are silently ignored — only `on.push` selects. A
workflow with *no* `push` trigger parses fine and simply never
matches (`no on.push workflow matched` → vacuous success).

## Execution parity (peeled workflows)

Once peeled, all three workflows **plan and execute**: jobs run
sequentially in `needs` topo-order, matrix combos fan out
sequentially, `actions/checkout` verifies HEAD, step
`env`/`working-directory`/`shell` are honored, and failing `run`
steps fail their job while independent jobs continue — matching
GHA's aggregate-status shape. In the scratch repos the cargo/npm
steps fail fast (no real project/toolchain), so the aggregate
exit is 1 — the *machinery* is what was verified.

A purpose-built subset workflow (matrix 2×2, `needs` chain,
workflow/job/step `env` with `${{ env.* }}`/`${{ matrix.* }}`/
`${{ github.* }}`, `defaults.run.working-directory` + `shell`,
step `working-directory` override, `shell: sh`,
`actions/checkout` with `with:`, `branches` + `paths-ignore`
filters) passes **fully green**, including correct skip of a
`needs`-dependent job when a matrix leg fails.

## Semantic gaps vs GitHub Actions

- **`$GITHUB_ENV` / `$GITHUB_OUTPUT` / `$GITHUB_PATH` /
  `$RUNNER_TEMP` are unset.** `run: echo X=1 >> $GITHUB_ENV` fails
  with `ambiguous redirect`. This is the biggest functional gap —
  ripgrep's cross-install step and anyhow's nightly RUSTFLAGS
  steps depend on it. Filed as **T-060** (linked to T-048).
- **`runs-on` is advisory.** `windows-latest`/`macos-latest` jobs
  execute locally on the watcher's Linux box — serde's `windows`
  job would "pass" having never run Windows. No platform guard or
  warning today.
- **Matrix is sequential** (documented): GHA runs combos in
  parallel; bupha runs them in order. ripgrep's 17-combo
  `include` matrix isn't even expressible (`include`/`exclude`
  unsupported) — cartesian list-form only.
- **No `if:` anywhere** — job and step conditions hard-error.
  This is the single most common GHA construct; until it lands,
  nearly every real workflow needs manual porting.
- **Third-party actions are the hard wall.** `actions/setup-*`,
  `dtolnay/rust-toolchain`, `actions/cache`,
  `actions/upload-artifact`, `docker://…` all error. Toolchain
  setup is the first thing every Rust CI does — `gha` v1 only
  helps workflows that are already pure `run:` + `checkout`.
- **Expression language is dotted-paths only.** No `==`, `&&`,
  `||`, ternary, `startsWith`, `hashFiles`, `format`, no
  `needs.*`/`steps.*`/`runner.*`/`strategy.*` contexts.
- **Reusable workflows** (`jobs.<id>.uses:`) unsupported.
- **`paths` filters diff `HEAD^..HEAD`** — tip commit only;
  multi-commit pushes undercount (documented boundary).
- **`actions/checkout` ignores `with.ref`/`fetch-depth`** — it is
  a verification no-op; the worktree is already at the pushed sha.
  `with.path` errors.
- **Parse-before-filter**: every candidate workflow file is parsed
  even when its `on.push` filters wouldn't match — a broken
  non-matching workflow still fails the spawn (loud by design,
  differs from GHA which wouldn't evaluate it at all).
- **No timeouts, services, containers, concurrency, outputs** —
  all hard errors (documented subset boundary).

## Bugs found & fixed in this dogfood

1. **`env:` null values rejected** — `KEY:` (YAML null) errored
   `expected a scalar, got null`; GHA coerces to `""`. Fixed in
   `env_map` + regression test.
2. **Step `with:` unreachable** — `with` was missing from the
   step key allowlist *and* the unsupported list, so
   `actions/checkout` `with:` inputs errored as `unknown key
   'with'` before `parse_uses` could validate them (the `with.path`
   check was dead code). `with` is now allowed on `uses` steps,
   rejected on `run` steps.
3. **Job `name:` never interpolated** — `${{ matrix.* }}` in
   `jobs.<id>.name` passed through verbatim into logs. Names are
   now interpolated like every other field; unsupported
   expressions in names now fail loudly (consistent with the
   subset contract).

## Reproduce

```bash
# scratch repo: git init, commit, drop a real ci.yml into
# .github/workflows/, then:
bupha --config bupha.yaml run   # runner.kind = "gha"
```

`spawn failed: gha: <file>:<path>: <reason>` names the exact YAML
key — peel iteratively to enumerate the full unsupported surface.
