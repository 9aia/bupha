# Contributing

Issues and PRs are **disabled** — the project is agent-maintained and
dogfoods its own CI. Fork freely under MIT.

## Layout

`crates/` — rust workspace. `bupha-core` is pure domain (queue,
config); ports are crates: `bupha-runner`, `bupha-tunnel`,
`bupha-trigger`, `bupha-status`; impls: `bupha-github` (trigger +
sink), `bupha-script`/`bupha-dagger` (runners), `bupha-cloudflared`
(tunnel), `bupha-deploy` (install helpers — `Service` port pending);
`bupha` (cli) is the composition root. `modules/dagger/` is the
shared pipeline module (Python SDK).

## Rules of thumb

- New runner/tunnel/trigger = a new impl crate, not a core change.
- `cargo build --workspace` and `cargo test --workspace` must stay green
  (unit tests + the e2e in `crates/bupha-cli/tests/` — no docker needed).
- Keep the binary dependency-light and sync; no async runtime.
- Commits: conventional prefixes (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`).
- Never commit secrets — `bupha.toml` takes names of env vars, not values.

## Releasing

Seed once with `bupha deploy release` (publishes workspace crates,
tags `{repo}-v{ver}`). Afterwards `scripts/release.sh` runs on each
`main` push via bupha's own dogfood loop — bump, publish, tag, dist
binaries. See `docs/reference/releasing.md`.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why). API reference
is generated from doc comments with
[cargo-rdme](https://github.com/orium/cargo-rdme): `cargo rdme` —
keep `lib.rs` module docs accurate; they feed the generated docs.
