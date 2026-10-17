# Contributing

Issues and PRs welcome at https://github.com/9aia/bupha.

## Layout

`crates/` — rust workspace. `bupha-core` holds the queue, config, and
the Trigger/StatusSink ports; ports `bupha-runner`/`bupha-tunnel` each
have impl crates (`bupha-dagger`, `bupha-script`, `bupha-cloudflared`);
`bupha-github` implements the trigger + status sink; `bupha-cli` is the
binary; `bupha-deploy` covers install-time helpers. `modules/dagger/`
is the shared pipeline module (Python SDK).

## Rules of thumb

- New runner/tunnel/trigger = a new impl crate, not a core change.
- `cargo build --workspace` and `cargo test --workspace` must stay green
  (unit tests + the e2e in `crates/bupha-cli/tests/` — no docker needed).
- Keep the binary dependency-light and sync; no async runtime.
- Commits: conventional prefixes (`feat:`/`fix:`/`refactor:`/`docs:`/`chore:`).
- Never commit secrets — `bupha.toml` takes names of env vars, not values.

## Releasing

Maintained with cargo-dist + release-plz; tags `v*` build binaries and
publish to crates.io.

## Docs

Follow Diátaxis: `docs/tutorials/` (learn) · `docs/how-to/` (tasks) ·
`docs/reference/` (lookup) · `docs/explanation/` (why). API reference
is generated from doc comments with
[cargo-rdme](https://github.com/orium/cargo-rdme): `cargo rdme` —
keep `lib.rs` module docs accurate; they feed the generated docs.
