# TODO — next version

## Hardening
- [ ] Webhook replay protection — dedup `X-GitHub-Delivery` IDs
      (bounded ring buffer ~10k entries) or timestamp window; GitHub
      sends neither, so a replayed signed payload re-enqueues today.
- [ ] Atomic `queue.json` writes — tmpfile + rename; a crash mid-write
      must not lose accepted work. (Multi-box/multi-process locking is
      out of scope for the single-node design.)
- [ ] Timeout on `post_status`/webhook HTTP calls (ureq) — a stalled
      GitHub API currently blocks the handler thread inside the lock.

## Tests
- [ ] Tests for `modules/dagger` — pytest for `_SELF_PATCH_PY` (fixture
      Cargo.tomls: append, idempotent re-run, strip-inside-markers,
      comment-safety); module smoke via `dagger call doctor` stays
      manual/integ for now.
- [ ] e2e for cancel/supersede path (needs a second signed push while a
      run is in flight — script runner makes this cheap).

## Distribution
- [ ] `cargo publish` (dep order) + tag v0.1.0 when the first full gate
      passes green on platform.
- [ ] GHES support — `api_base` on `GitHubStatus`/`register_webhook`.
- [ ] `bupha init` should emit a fuller delegate template + optional
      non-rust skeleton for `runner.kind = "script"`.

## Nits
- [ ] `modules/dagger` dev-link exec has a dead `elif` checking the same
      path as the `if` — dedupe.
- [ ] `.github/workflows/` exists for releases only — fine, but keep
      docs noting release-GHA ≠ project-CI to prevent confusion.

## Queue / jobs
- [ ] Per-repo queue policy: `queue = "latest"` (current) vs
      `"all"` (FIFO, run every push — VecDeque instead of pending slot).
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
