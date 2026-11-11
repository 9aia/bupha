# TODO — next version (post-0.1.0)

- [ ] T-027 status signature/attestation link in commit-status body (ADR-0002 compliance) · status:in-progress · created:2026-10-31
- [ ] T-031 presentation deck — pending: run via ~/.9aia-pres pipeline (pres kick bupha or supervisor); agent writes presentation/ on branch presentation/initial, slidev build verify, push; no PR (sturn flow) — the pushed branch is the deliverable; merge is a human/sturn call; status board: ~/.9aia-pres/STATUS.md · created:2026-11-02
- [ ] T-032 agent-awareness managed block — bupha's repo contract (myco check in gate pipelines, Item: T-NNN commit trailers) should live in a \<!-- bupha:agent-awareness --> block written by init + validated by check — pattern: myco init/check · created:2026-11-02
- [ ] T-044 rotate CARGO\_REGISTRY\_TOKEN — was at .env 0644 (now 600); treat as exposed · pri:high · created:2026-11-03
- [ ] T-049 migrate queue/cache/lease impls out of bupha — redb/redis queue becomes hypha-side impl; bupha keeps thin port or none (ADR-0006) · pri:high · created:2026-11-10
- [ ] T-050 gha self-hosted runner — speak GHA runner protocol so repos can runs-on: \[self-hosted, bupha\] (ADR-0006 bidirectional gha) · created:2026-11-10
- [ ] T-054 gha naming collision — runner.kind=gha (dialect) vs self-hosted GHA runner (T-050) are opposite directions; rename one · pri:high · created:2026-11-10
- [ ] T-055 webhook ingress ownership — driver hooks register vs peregrin campaign listeners; decide single owner per ADR-0006 · pri:high · created:2026-11-10
- [ ] T-056 secrets consolidation — pick one story: cuora broker vs BUPHA\_\* env names vs ~/.config/bupha/env file · pri:high · created:2026-11-10
- [ ] T-057 docs/README/skill sweep — drop bupha CLI story (watch/deploy/run/doctor/import) per ADR-0006 amendment · pri:high · created:2026-11-10
