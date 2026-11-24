# Changelog

## [0.3.0] - 2026-11-24

### Added

- shared pull queue + bupha worker + label routing (T-005) (`e22f602`)
- [cache] config section + RunHandle on Runner port (`cab5eac`)
- five new port/impl pairs extracted (`2833c11`)

### Changed

- queue persistence → redb (embedded, ACID) (`e27dd1a`)
- **queue:** retire redb/redis/lease queue impls; entity worker ref gate (T-049, T-068) (`09033ca`)

### Fixed

- multi-watcher queue bugs — restore, scoped persist, done markers, claim-lost kill (T-005) (`2aa7398`)
