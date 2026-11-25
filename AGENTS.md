<!-- myco:agent-awareness -->

## Agent awareness

- Session start / any work discussion: run `myco status --prompt`
  before proposing next steps — know the board first.
- User describes new work → offer to capture it (`myco todo add`,
  `issue new`, `proposal new`). Don't wait to be asked.
- Picking up a task: `myco resume <ID>` is the briefing.
- Wrapping up: `myco handoff <ID>` anything resumable; keep statuses
  honest (claimed → in-progress → blocked/done).

<!-- /myco:agent-awareness -->

<!-- sturn:agent-awareness -->

## Sturn awareness

- **No PRs.** The pushed branch/commit is the deliverable; review
  streams continuously (sturn WAL), merge is a human/sturn call.
- sturn-wired repo (`sturn.toml` present): `sturn claim "<glob>"
  --ws <ws>` *before* edits — claims surface conflicts in ms.
- Ops via `sturn write|rename|delete`; `sturn snap` folds the ws
  into a real commit; `sturn push` publishes heads + refs/sturn/*.
- `sturn leave` on exit — drops the actor's claims.

<!-- /sturn:agent-awareness -->

<!-- kuda:agent-awareness -->

## Kuda awareness

- `.kuda/` cards are committed artifacts — read them before
  re-exploring (`kuda map`, `kuda card <mod>`); `kuda status`
  reports per-card drift vs HEAD.
- Code changes that invalidate a card → regenerate it (`kuda
  sync`) in the same change; cards ride the diff they describe.
- `.kuda/index.sqlite` is gitignored + rebuildable; the `*.yaml`
  cards are the checked-in source of truth.

<!-- /kuda:agent-awareness -->

<!-- bupha:agent-awareness -->

## Bupha awareness

- This repo is gated by bupha — pushes to the watched branch run
  the configured pipeline in a detached worktree; keep it green.
- Gate pipelines should run `myco check` — ledger hygiene is part
  of CI, not a manual step.
- Commits that move a ledger item carry an `Item: T-NNN` trailer
  naming the myco item — runs and history stay attributable.
- `bupha init <dir>` scaffolds the gate and writes this block;
  `bupha doctor` validates it.

<!-- /bupha:agent-awareness -->

# Bupha

CI integration for tamias — **not a standalone product**: fleets
ship as `type: bupha` tamias bundles, `tamias install`/`deploy`
provisions the watcher, tunnel, and forge hooks, and
`tamias-driver-bupha host` runs the watcher engine in-process under
the provider unit (ADR-0006). The `bupha` command exists only as a
dev/debug harness behind the `libraries/bupha` pack's `bins[]` shim
(`bupha-cli` is the engine lib — no `[[bin]]`, T-012). Production
webhook/forge ingress is a peregrin campaign listener (ADR-0007);
the in-repo `/hook` listener is the standalone/dev fallback.
`action.yml` is a composite GitHub Action that reads the forge's
commit-status API (`mode: status|wait`).

## Layout

Flat `crates/` workspace, ports-and-adapters. `bupha-core` is pure
domain (queue transitions, config model); every concern is a port
trait with impl crates:

| Crate | Role |
| --- | --- |
| `bupha-core` | queue semantics, config model — pure, no I/O |
| `bupha-cli` (pkg `bupha`) | the engine lib — clap, `--json`, `manifest`/`schema`, `run_from()` for drivers; no `[[bin]]` |
| `bupha-driver` | `tamias-driver-bupha` — pavo views + engine ops surface + `host`/`worker`/`hooks`/`cli` verbs |
| `bupha-watch` | the watcher — sources → queue → runners → status sinks; also hosts standalone/dev ingress helpers (`tunnel`/`webhook` modules) |
| `bupha-trigger` / `bupha-sturn` | `Source` port + impls — GitHub webhook deliveries and the sturn WAL poller |
| `bupha-queue` | work-queue port (per-key lists + leases); `JsonQueueStore` dev impl, durable state is the `ci-job` entity plane |
| `bupha-plane` | `hypha_http::EntityPlane` impl — the bupha↔hypha/peregrin seam for the `ci-job` plane |
| `bupha-peregrin` | peregrin-side ci-job integration — entity plane composition, fleet specs, hub rehydrate, kind policy (P-047/PL-028) |
| `bupha-runner` + `-shell`/`-dagger`/`-ssh`/`-container`/`-http`/`-gha` | `Runner` port + impls — local shell, dagger pipelines, remote ssh, podman/docker isolation, remote `bupha agent`, native GHA workflow dialect |
| `bupha-gha-runner` | registers this box as a GitHub Actions self-hosted runner (own config, not `runner.kind`) |
| `bupha-github` | forge impl — webhooks in, commit statuses out |
| `bupha-status` | status-sink port |
| `cuora-secret*` (git deps) | secrets chain — env → `cuora://bupha/<NAME>` (+ pack scope) → dotenv file (P-043; the `bupha-secret-*` crates are gone) |
| `bupha-access` + `-file`/`-forge` | `Access` port — sender authorization (forge-attested `sender`, not self-reported) |
| `bupha-attest` + `-file`/`-sigstore` | run attestation — sha/repo/runner/status/wall predicates |
| `bupha-audit-file` | audit-event sink |
| `bupha-history` | bounded `history.jsonl` run log |
| `bupha-cache` + `-sccache` | artifact-cache port + sccache impl |
| `bupha-config-file` | `bupha.toml`/`bupha.yaml` config impl (`~/` expansion on local paths) |
| `bupha-watch::telemetry` | domain `RunEvent`/`Telemetry` port — OpenMetrics textfile sink is `lampy-metrics-file` (9aia/lampy, P-042) |
| `bupha-http` | `bupha agent` wire — remote job submit/worktree/status over HTTP |
| `libraries/` | tamias packs — `bupha` (command), `ci-gate`, `ci-ingress`, `docs-deploy`, `gha-runner` (`type: bupha` bundles) |
| `modules/dagger/` | shared pipeline module (Python SDK) |
| `scripts/` | `pack.sh` (stage driver payload), `coverage.sh`, `gha-status.sh` |

## Commands

No xtask — plain cargo + scripts:

```bash
cargo build --workspace && cargo test --workspace   # unit + e2e
                                               # (e2e spawn tamias-driver-bupha)
scripts/coverage.sh            # llvm-cov HTML → target/llvm-cov/html;
                               # extra args pass through (--fail-under-lines 100)
scripts/pack.sh                # release tamias-driver-bupha →
                               # libraries/bupha/drivers/bin/<triple>/
tarsi-serve release --dir . --yes
                               # operator-run: version bump + tag
                               # (dry-run without --yes)
```

The `bupha` command itself is the pack shim — stage with
`scripts/pack.sh`, deploy with `tamias install --allow-drivers
./libraries/bupha`.

## Boundary

bupha owns **CI gating** — push/PR ingest, queueing, worktree
isolation, pipeline execution, forge status write-back. Neighbors:

- **tamias** — the product surface; bupha is a `type: bupha`
  content integration (packs, drivers, provider units).
- **peregrin** — orchestration; production forge ingress is a
  peregrin campaign listener normalizing deliveries into
  `ci-job` entities (ADR-0007). peregrin also owns all gate
  *semantics*: the repo's `workflows/gate.yaml` campaign; the
  `run.result` artifact is the contract (bupha maps `conclusion` →
  forge commit status; `ci.sh` is retiring). bupha's own `github`
  source is the standalone/dev fallback — in production bupha never
  parses forge payloads.
- **lasius** — the shared ports: `lasius-forge` (commit-status
  writes, check reads, PR ops) and `lasius-vcs`. Forge status
  write-back goes *through* `lasius-forge` — bupha doesn't shell
  `gh` itself; lasius owns the forge API surface, not CI semantics.
- **hypha** — the entity plane; `ci-job` durable queue state lives
  there via `bupha-plane`.
- **cuora** — secrets (`cuora://bupha/<NAME>` is canonical).
- **sturn** — the WAL source reads `refs/sturn/wal/*`; fleet claims.
- **myco** — this repo's `project/` ledger; `Item: T-NNN` trailers.
- **kuda/mimus** — code memory / critic for this repo.

A CI server product (UI, YAML DSL, managed fleet) is explicitly out
of scope.

## Docs

`docs/` worktree — VitePress + Diátaxis (tutorials/how-to/reference/
explanation) → GitHub Pages at `9aia.com/bupha`; plus `comparison.md`
(the GHA comparison table the README links). `cypera gen` (driven by `cypera.toml`)
renders cli/config/crate-map/crate-api from source — keep `lib.rs`
module docs accurate.

## Specs

`specs/<name>.toml` is this repo's mimus-standard conformance spec —
approval-gated. Any change requires a live org proposal
(`proposal-NNNN`, status open|done) referenced in the spec header or
commit range, and no coverage regression vs merge-base. Guard:
workspace `../../scripts/spec-guard.py`; sweep:
`../../scripts/standard.sh`; regenerate coverage only via
`../../scripts/gen-repo-specs.py`.
