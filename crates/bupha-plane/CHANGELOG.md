# Changelog

## [0.3.0] - 2026-11-24

### Added

- **plane:** ci-job rehydrate from entity index — hub projection is the store, plane rebuilds lanes+claims on boot (T-072) (`de1419f`)
- **plane:** bupha-plane — ci-job entity plane over hypha EntityPlane (P-002, T-049 slice 1) (`ceddf85`)
- **ci-job:** fork-PR head checkout via cloneUrl/headRef (T-073) (`882a92c`)
- **plane:** fleet normalization at ci-job create — with_watched specs resolve queue/runsOn/runner/context/maxAttempts, gate ref+event (T-067) (`5f560da`)

### Changed

- **queue:** retire redb/redis/lease queue impls; entity worker ref gate (T-049, T-068) (`09033ca`)
