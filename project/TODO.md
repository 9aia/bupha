# TODO — next version (post-0.1.0)

## Post-publish
- [ ] `MODULE_SHA` — bump pinned dagger install to the release tag.
- [ ] `CHANGELOG.md` — 0.1.0 entry (manual now, generated later).
- [ ] README badges — crates.io + license (resolve green once live).

## Queue / jobs
- [ ] `[[repo.jobs]]` — multiple pipelines per repo, each with own
      queue policy + runner (e.g. `ci` gate + debounced `release`).
- [ ] Distributed runners — `bupha-ssh` lands first; remote trigger
      impl (`bupha-http`?) for cross-box fan-out; unblocks multi-arch
      + release.sh's "other arches" dist step.

## Tests
- [ ] e2e for cancel/supersede path (second signed push in flight).
