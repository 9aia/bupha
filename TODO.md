# TODO — next version

## Hardening
- [ ] Webhook replay protection — dedup `X-GitHub-Delivery` IDs
      (bounded ring buffer ~10k entries) or timestamp window; GitHub
      sends neither, so a replayed signed payload re-enqueues today.
- [ ] Atomic `queue.json` writes — tmpfile + rename; a crash mid-write
      must not lose accepted work. (Multi-box/multi-process locking is
      out of scope for the single-node design.)
- [ ] Timeout on `post_status`/webhook HTTP calls (ureq) — a stalled
      GitHub API still stalls the handler thread.

## Tests
- [ ] Tests for `modules/dagger` — pytest for `_SELF_PATCH_PY` (fixture
      Cargo.tomls: append, idempotent re-run, strip-inside-markers,
      comment-safety); module smoke via `dagger call doctor` stays
      manual/integ for now.
- [ ] e2e for cancel/supersede path (needs a second signed push while a
      run is in flight — script runner makes this cheap).

## Distribution
- [ ] Seed publish: `git tag bupha-v0.1.0` → the watch loop
      publishes touched crates + dist binaries automatically.
- [ ] GHES support — `api_base` on `GitHubStatus`/`register_webhook`.
- [ ] `bupha init` should emit a fuller delegate template + optional
      non-rust skeleton for `runner.kind = "script"`.

## Nits
- [ ] `modules/dagger` dev-link exec has a dead `elif` checking the same
      path as the `if` — dedupe.
- [x] ~~`.github/` workflows for releases~~ — deleted; releases run
      on bupha itself via scripts/release.sh.

## Queue / jobs
- [x] ~~Per-repo queue policy `latest`|`all`~~ — shipped (VecDeque,
      bounded, evicts-oldest).
- [ ] `[[repos.jobs]]` — multiple pipelines per repo, each with own
      queue policy + runner (e.g. `ci` gate + debounced `release`).

## Later
- [ ] Remote/distributed runners: `bupha-webhook` runner (POST signed
      job to another bupha) + `bupha-http` trigger (generic signed job
      endpoint, not github-shaped) — enables multi-arch builds on
      worker boxes without GHA.
- [ ] `runner.kind = "shell"` — inline shell strings (vs script file
      `cmd`), or alias; decide semantics.
- [ ] Trigger impls: gitea/gitlab.
- [ ] Runner impls: earthly, nix.
