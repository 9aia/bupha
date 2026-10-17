# TODO — next version (post-0.1.0)

## Queue / jobs
- [ ] `[[repos.jobs]]` — multiple pipelines per repo, each with own
      queue policy + runner (e.g. `ci` gate + debounced `release` —
      makes today's release.sh runner-override a first-class job).

## Tests
- [ ] e2e for cancel/supersede path (a second signed push while a run
      is in flight — script runner makes this cheap).

## Distribution
- [ ] GHES support — `api_base` on `GitHubStatus`/`register_webhook`.
- [ ] `bupha init` fuller delegate template + optional non-rust
      skeleton for `runner.kind = "script"`.
- [ ] `doctor` checks for release.sh deps (cargo-edit, dist, gh) when
      a repo's runner cmd is a release script.

## Nits
- [ ] `modules/dagger` dev-link exec has a dead `elif` checking the
      same path as the `if` — dedupe.
- [ ] `repos.dir` relative paths — canonicalize at load so the unit
      isn't cwd-sensitive.
- `bupha doctor --repo <path>` — also run `dagger call doctor` on the
  consumer repo (catches pipeline-level gaps, not just box gaps)
- `Vcs` port — extract `git()`/`repo_sync` behind a trait (`GitVcs` impl);
  unblocks jj/hg + makes git ops unit-testable
- `QueueStore` port — `queue.json` behind `load/persist`; SQLite/remote
  store is the distributed-runner prerequisite
- `Service` port in bupha-deploy — systemd today, launchd/Windows later
- `keyring`-crate secret backends + `dirs`-crate config paths (macOS/Win)
- Cache portability — today `sccache_conf` covers sccache's own
  backends; add a generic `cache` config seam so file-based/S3/R2/GCS
  stores are a config pick (pluggable backend trait)
