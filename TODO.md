# TODO — next version (post-0.1.0)

## Post-publish

- [ ] T-001 MODULE\_SHA — bump pinned dagger install to the release tag.
- [ ] T-002 CHANGELOG.md — 0.1.0 entry (manual now, generated later).
- [ ] T-003 README badges — crates.io + license (resolve green once live).
- [ ] T-024 \[\[repo]] retry field — failed runs retry with backoff (ADR-0002 SLA surface, untracked) · created:2026-10-31
- [ ] T-025 bupha-audit-file sink — access-decision audit log (ADR-0002, untracked) · created:2026-10-31
- [ ] T-026 bupha doctor --json — machine-readable check output (ADR-0002 support surface) · created:2026-10-31
- [ ] T-027 status signature/attestation link in commit-status body (ADR-0002 compliance) · created:2026-10-31
- [ ] T-006 e2e for cancel/supersede path (second signed push in flight). · status:in-progress

## Queue / jobs

- [ ] T-005 Distributed runners — bupha-ssh lands first; remote trigger impl (bupha-http?) for cross-box fan-out; unblocks multi-arch
  - [ ] T-022 release.sh's "other arches" dist step.
