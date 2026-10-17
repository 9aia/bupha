# Releasing

`cargo-dist` + `release-plz` drive releases (GitHub Actions — note:
bupha *replaces* GHA as its own CI; the release workflows here build
bupha's distributable artifacts and publish crates, a different job).

- Tag `v*` → `.github/workflows/release.yml` builds binaries
  (linux/macos/windows, x86+arm) + a `curl|sh` installer and attaches
  them to the GitHub Release.
- Every merge to `main` → `release-plz` opens/updates a version-bump
  PR; merging it publishes crates to crates.io in dependency order
  (core → ports → impls → cli `bupha`).
- Requires repo secret `CARGO_REGISTRY_TOKEN`.
