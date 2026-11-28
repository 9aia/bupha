# TODO — next version (post-0.1.0)

- [ ] T-066 gha-runner live validation — run against a real repo's self-hosted runner (registration token → session → acquirejob → completejob → results service) on github.com; check GHES api-version drift · created:2026-11-11 · links:T-050
- [ ] T-078 ci-job entities don't survive node restarts — CiJobPlane is in-memory, rehydrate reads hub entity-index but hub projection contains zero ci-job docs (sync broken/kind not projected); every node restart silently drops the CI queue · status:in-progress · pri:high · owner:devin · created:2026-11-28 · #ci · #durability

## open
