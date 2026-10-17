# TODO — next version (post-0.1.0)

## Post-publish
- [ ] `MODULE_SHA` — bump pinned dagger install to the release tag.
- [ ] `CHANGELOG.md` — 0.1.0 entry (manual now, generated later).
- [ ] README badges — crates.io + license (resolve green once live).

## Ports (seams — see architecture.md)
- [x] `Vcs` port — `git()`/`repo_sync` behind a trait (`GitVcs`);
      unblocks jj/hg + makes git ops unit-testable.
- [x] `QueueStore` port — `queue.json` behind load/persist;
      SQLite/remote store = distributed-runner prerequisite.
- [x] `Service` port in bupha-systemd — systemd today, launchd/Windows.
- [x] `keyring`-crate secret backends + `dirs`-crate config paths
      (macOS/Windows).
- [ ] Cache seam — generic `cache` config so file/S3/R2/GCS are a
      config pick (today `sccache_conf` only).
- [ ] `bupha doctor --repo <path>` — also run `dagger call doctor` on
      the consumer repo (pipeline gaps, not just box gaps).

## Queue / jobs
- [ ] `[[repo.jobs]]` — multiple pipelines per repo, each with own
      queue policy + runner (e.g. `ci` gate + debounced `release`).
- [ ] Remote/distributed runners — `bupha-webhook` runner +
      `bupha-http` trigger; unblocks multi-arch + release.sh's
      "other arches" dist step.

## Tests
- [ ] e2e for cancel/supersede path (second signed push in flight).

## Distribution / nits
- [ ] GHES support — `api_base` on `GitHubStatus`/`register_webhook`.
- [ ] `bupha init` fuller delegate + optional non-rust skeleton for
      `runner.kind = "shell"`.
- [ ] `doctor` checks release.sh deps (cargo-edit, dist, gh) when a
      repo's runner cmd is a release script.
- [ ] `modules/dagger` dev-link exec dead `elif` — dedupe.
- [ ] `repos.dir` relative paths — canonicalize at load (cwd-safe).
