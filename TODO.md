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
- [ ] Move `post_status` calls out of the shared-queue lock — a slow
      GitHub API serializes enqueue/cancel/worker transitions.

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

## Later
- [ ] Trigger impls: gitea/gitlab.
- [ ] Runner impls: earthly, nix.
- [ ] Per-repo runner overrides in `[[repos]]`.
