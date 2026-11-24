# Changelog

## [0.3.0] - 2026-11-24

### Added

- **plane:** fleet normalization at ci-job create — with_watched specs resolve queue/runsOn/runner/context/maxAttempts, gate ref+event (T-067) (`5f560da`)

### Changed

- bupha-config-toml → bupha-config-file — file impl of ConfigSource (`5707e87`)
- **bupha-config-file:** scarab-config-file at a68fed2, serde_yaml_ng YAML (P-045) (`0b2ff3b`)
- split config adapters — bupha-config-{toml,yaml} per-syntax deps + file dispatcher (`03524d6`)
