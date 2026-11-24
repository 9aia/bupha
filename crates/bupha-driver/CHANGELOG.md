# Changelog

## [0.3.0] - 2026-11-24

### Added

- **bundles:** worker service in ci-gate/docs-deploy + [worker] node config (T-064) (`bded75e`)
- **bupha-driver:** dual-surface tamias content driver (`acce13b`)
- **secrets:** cuora broker is canonical — bupha-secret-cuora impl + chain rewire (T-056) (`aa68d7e`)
- **driver:** engine ops surface — ops manifest, run passthrough, cli/mcp (`9cdfa72`)
- **bupha-driver:** cli verb is an engine passthrough — the tamias bins[] shim contract (`6b26d79`)
- tamias-driver-bupha — pavo view driver over bupha's on-disk CI state (`58f93fb`)
- **worker:** tamias-driver-bupha worker — ci-job entity-plane claim loop (P-002, T-062) (`2cdcafb`)
- **driver:** host verb + bupha-watch lib — units exec driver binary, not bupha CLI (T-052, ADR-0006) (`199ab36`)

### Changed

- **bupha-driver:** adopt pavo-driver-kit (`9d80077`)
- **secrets:** retire bupha-{secret,keyring,dotenv} — hypha impls + cuora canonical (T-077) (`99f92c2`)
- **service:** retire bupha-{service,systemd} — hypha-systemd + cli deploy module (T-077) (`6222c8a`)
- service lifecycle onto tamias-deploy-systemd (PL-025) (`611bd9f`)
- **secret:** secrets chain on cuora-secret crates (P-043) (`3ee081c`)
- **queue:** retire redb/redis/lease queue impls; entity worker ref gate (T-049, T-068) (`09033ca`)

### Fixed

- **init:** dagger init failure warns instead of aborting — AGENTS.md block always converges (`605e17c`)
- **release:** retire crates.io publish — tag-only tamias pack release (`04380a1`)
