# TODO — next version (post-0.1.0)

## Post-publish

- [ ] T-001 MODULE\_SHA — bump pinned dagger install to the release tag.
- [ ] T-002 CHANGELOG.md — 0.1.0 entry (manual now, generated later).
- [ ] T-003 README badges — crates.io + license (resolve green once live).
- [ ] T-024 \[\[repo]] retry field — failed runs retry with backoff (ADR-0002 SLA surface, untracked) · created:2026-10-31

## Queue / jobs

- [ ] T-004 \[\[repo.jobs]] — multiple pipelines per repo, each with own queue policy + runner (e.g. ci gate + debounced release ).
- [ ] T-005 Distributed runners — bupha-ssh lands first; remote trigger impl (bupha-http?) for cross-box fan-out; unblocks multi-arch
  - [ ] T-022 release.sh's "other arches" dist step.

## Tests

- [ ] T-006 e2e for cancel/supersede path (second signed push in flight).
