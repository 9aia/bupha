# TODO — next version (post-0.1.0)

- [ ] T-027 status signature/attestation link in commit-status body (ADR-0002 compliance) · status:in-progress · created:2026-10-31
- [ ] T-031 presentation deck — pending: run via ~/.9aia-pres pipeline (pres kick bupha or supervisor); agent writes presentation/ on branch presentation/initial, slidev build verify, push; no PR (sturn flow) — the pushed branch is the deliverable; merge is a human/sturn call; status board: ~/.9aia-pres/STATUS.md · created:2026-11-02
- [ ] T-032 agent-awareness managed block — bupha's repo contract (myco check in gate pipelines, Item: T-NNN commit trailers) should live in a \<!-- bupha:agent-awareness --> block written by init + validated by check — pattern: myco init/check · created:2026-11-02
- [ ] T-044 rotate CARGO\_REGISTRY\_TOKEN — was at .env 0644 (now 600); treat as exposed · pri:high · created:2026-11-03
- [ ] T-045 tamias-driver-bupha — pavo view driver (manifest/views/view/query over ndjson; refs: kuda-driver, myco-driver, sturn-driver, cuora-driver, tamias-driver-tamias, mimus-driver; must pass pavo-testkit check\_driver; read-only views of bupha's natural surfaces; P-003/PL-004) · created:2026-11-08
