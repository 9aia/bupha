# TODO — next version (post-0.1.0)

- [ ] T-027 status signature/attestation link in commit-status body (ADR-0002 compliance) · status:in-progress · created:2026-10-31
- [ ] T-031 presentation deck — pending: run via ~/.9aia-pres pipeline (pres kick bupha or supervisor); agent writes presentation/ on branch presentation/initial, slidev build verify, push; no PR (sturn flow) — the pushed branch is the deliverable; merge is a human/sturn call; status board: ~/.9aia-pres/STATUS.md · created:2026-11-02
- [ ] T-032 agent-awareness managed block — bupha's repo contract (myco check in gate pipelines, Item: T-NNN commit trailers) should live in a \<!-- bupha:agent-awareness --> block written by init + validated by check — pattern: myco init/check · created:2026-11-02
- [ ] T-036 SEC: agent auth binds method+path+body — kills empty-body replay (I-005) · created:2026-11-03
- [ ] T-037 SEC: verify sturn event sigs when allowed\_signers configured; unattested sender '' (I-006) · created:2026-11-03
- [ ] T-038 SEC: ssh runner input allowlists — remote/tag/env-name (I-007) · created:2026-11-03
- [ ] T-039 SEC: doctor/deploy-check perms on env + sccache conf; chmod local files (I-008) · created:2026-11-03
- [ ] T-040 SEC: webhook request-thread cap + POST-only /hook (I-009) · created:2026-11-03
- [ ] T-041 SEC: forge-access URL-encode + no negative cache (I-010) · created:2026-11-03
- [ ] T-042 SEC: worker skips unknown fleet job keys (I-011) · created:2026-11-03
- [ ] T-043 SEC: sanitize patch\_host in init template (I-012) · created:2026-11-03
