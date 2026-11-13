# Contributing

Issues and PRs are **disabled** — the project is agent-maintained and
dogfooded on its own CI (the org gate runs on bupha itself). Fork
freely under MIT.

## Layout

`crates/` — rust workspace. `bupha-core` is pure domain (queue,
config); ports are crates: `bupha-runner`, `bupha-trigger`,
`bupha-status`; impls: `bupha-github` (trigger +
sink), `bupha-shell`/`bupha-dagger`/`bupha-ssh` (runners);
`bupha-watch` also hosts the standalone/dev-mode ingress helpers
(`tunnel`/`webhook` modules — T-077 folded the ex-`bupha-tunnel`,
`-cloudflared`, `-webhook` crates in). Generic infra is *not*
bupha's: secrets resolve env → cuora (`bupha-secret-cuora`) →
`hypha-dotenv` fallback (the keyring leg was dropped post-T-077 —
`deploy env` migrates pre-cuora keyring entries into cuora), and systemd units go
through `hypha-systemd` (T-077 — the `bupha-{secret,keyring,
dotenv,service,systemd}` crates are retired);
`bupha` (package `bupha`, crate dir `bupha-cli`) is the engine
lib — no `[[bin]]`: the `bupha` command ships as the tamias pack
in `libraries/bupha` (`bins[]` shim → `tamias-driver-bupha cli` →
`bupha::run_from`, T-012); deployed fleets run the engine via
`tamias-driver-bupha host`/`worker` (ADR-0006). `modules/dagger/`
is the shared pipeline module (Python SDK).

## Rules of thumb

- New runner/tunnel/trigger = a new impl crate, not a core change.
- `cargo build --workspace` and `cargo test --workspace` must stay green
  (unit tests + the engine e2e in `crates/bupha-driver/tests/` —
  no docker needed; they spawn `tamias-driver-bupha run`).
- Coverage: `scripts/coverage.sh` — cargo-llvm-cov HTML report in
  `target/llvm-cov/html`; pass `--fail-under-lines 100` to gate.
- Keep the binary dependency-light and sync; no async runtime.
- Commits: conventional prefixes (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`).
- Never commit secrets — `bupha.toml` takes names of env vars, not values.

## Releasing

`scripts/release.sh` runs on each `main` push via bupha's own
dogfood loop (the dev CLI's `bupha deploy release` remains the
manual seed). A release is one workspace version bump
(`cargo set-version --workspace`), a sync of the version refs in
`libraries/bupha/tamias.yaml`/`README.md`, and a `bupha-v<ver>`
tag. There is **no crates.io publish**: the bupha crates are
internal lib-only engines (org policy keeps every repo but tamias
off crates.io). User-facing installs ride the tamias pack —
after the tag, the operator runs `tamias publish
libraries/bupha` to ship `9aia:bupha@<ver>` to the org registry.
See `docs/explanation/release-model.md`.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why). API reference
is generated from doc comments with
docs codegen: `docs/gen-*.py` renders config/crate-map/crate-api
from source (`bupha gen-cli-docs` renders cli.md via
clap-markdown) — keep `lib.rs` module docs accurate; they feed
the generated docs.
