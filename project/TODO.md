# TODO — next version (post-0.1.0)

## Post-publish
- [ ] `MODULE_SHA` — bump pinned dagger install to the release tag.
- [ ] `CHANGELOG.md` — 0.1.0 entry (manual now, generated later).
- [ ] README badges — crates.io + license (resolve green once live).

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
