# Changelog

## [0.3.0] - 2026-11-24

### Added

- **worker:** emit ci.job.completed to node event-ingest on settle (PL-015/0.1b) (`dff9d4b`)
- **secrets:** cuora broker is canonical — bupha-secret-cuora impl + chain rewire (T-056) (`aa68d7e`)
- **ci-job:** fork-PR head checkout via cloneUrl/headRef (T-073) (`882a92c`)
- **gha:** per-job commit statuses — one status per expanded workflow job/matrix leg (`67f4bcb`)
- **status:** attestation link on terminal commit status (T-027, ADR-0002 §5) (`2f8a064`)
- **worker:** tamias-driver-bupha worker — ci-job entity-plane claim loop (P-002, T-062) (`2cdcafb`)
- **driver:** host verb + bupha-watch lib — units exec driver binary, not bupha CLI (T-052, ADR-0006) (`199ab36`)
- **watch:** claim via submit_op_result — fence from op body, drop re-read (`17cf138`)

### Changed

- **vcs:** consume lasius-vcs port — drop bupha-vcs/bupha-git (`c7c717d`)
- **secrets:** retire bupha-{secret,keyring,dotenv} — hypha impls + cuora canonical (T-077) (`99f92c2`)
- **secret:** secrets chain on cuora-secret crates (P-043) (`3ee081c`)
- **ingress:** fold bupha-{tunnel,cloudflared,webhook} into bupha-watch (T-077) (`255c0eb`)
- move telemetry port into bupha-watch, sink to lampy (P-042/PL-023) (`1dc22ca`)
- **queue:** retire redb/redis/lease queue impls; entity worker ref gate (T-049, T-068) (`09033ca`)
