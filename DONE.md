# Done

- T-051 ci-job entity kind + ops contract (ci.enqueue/ci.status/ci.cancel) — seam between bupha driver and peregrin campaigns · pri:high · created:2026-11-10
- T-053 per-crate scope audit — rule each stateful crate keep-port/migrate-to-hypha/die: queue{,-lease,-redb,-redis} cache{,-sccache} service systemd tunnel cloudflared webhook trigger sturn secret keyring dotenv (ADR-0006) · pri:high · created:2026-11-10
- T-052 driver host mode — tamias-driver-bupha gains engine-host verb so units exec the driver binary, not ~/.cargo/bin/bupha (ADR-0006 no-CLI); interim: bundle payload or documented cargo-install · pri:high · created:2026-11-10
- T-055 webhook ingress ownership — driver hooks register vs peregrin campaign listeners; decide single owner per ADR-0006 · pri:high · created:2026-11-10
- T-054 gha naming collision — runner.kind=gha (dialect) vs self-hosted GHA runner (T-050) are opposite directions; rename one · pri:high · created:2026-11-10
- T-056 secrets consolidation — pick one story: cuora broker vs BUPHA\_\* env names vs ~/.config/bupha/env file · pri:high · created:2026-11-10
- T-057 docs/README/skill sweep — drop bupha CLI story (watch/deploy/run/doctor/import) per ADR-0006 amendment · pri:high · created:2026-11-10
- T-062 tamias-driver-bupha worker verb — entity-index claim loop replacing WorkQueue (P-002 §4.1): list ci-job entities via hypha-node-api HttpEntityApi, ci.claim/renew/complete; unblocks T-052 worker host side · pri:high · created:2026-11-11 · links:T-049,T-052,P-002
- T-050 gha-runner: bupha as GitHub self-hosted runner — speak actions/runner broker protocol so repos can runs-on: \[self-hosted, bupha\]; name reserved by T-054 — dialect keeps runner.kind=gha (ADR-0006 bidirectional gha) · created:2026-11-10
- T-061 peregrin-node: register CiJobPlane behind optional bupha feature (P-002 §4.2) — state.kinds.register(Arc::new(CiJobPlane::new().with\_labels(...))) next to CampaignEntityPlane; or stand up bupha-node dev binary composing hypha-node + plane · pri:high · created:2026-11-11 · links:T-049,T-058,P-002
- T-058 peregrin ci-ingress: campaign listener normalizes forge webhook → ci.enqueue (generic forge-trigger → ci-job bridge) · pri:high · owner:devin · created:2026-11-11 · links:T-055,P-002,ADR-0007
- T-063 ci-ingress normalization → POST /api/entities (T-058): forge webhook → bupha Trigger Push → ci-job indexDoc create; depends on T-061 plane registration · pri:high · created:2026-11-11 · links:T-049,T-058,P-002
- T-064 wire tamias-driver-bupha worker as provider.services\[] entry + \[worker] node/node\_token\_env in fleet.toml (P-002/T-062 follow-up) · owner:devin · created:2026-11-11
- T-059 cuora secrets migration ops + docs sweep — move live BUPHA\_\* values into cuora://bupha/\* (cuora put), drop ~/.config/bupha/env + env\_file once verified, update docs/ site pages (docs branch) that still describe env→keyring→file as canonical · created:2026-11-11 · links:T-056
