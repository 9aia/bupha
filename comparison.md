---
url: /bupha/comparison.md
---
# bupha vs hosted CI

bupha runs push-triggered CI on self-controlled hardware —
not a GHA replacement at fleet scale.

## vs GitHub Actions

| | bupha | GitHub Actions |
|---|---|---|
| compute | your box | managed fleet |
| queue | latest-wins + bounded `all`, no cold start | unlimited parallelism, queue wait |
| secrets | never leave the machine | hosted on GitHub |
| cache | your backend — local disk or your S3/R2/minio | hosted cache |
| pipeline def | your runner — `ci.sh` or dagger module | YAML DSL + marketplace actions |
| local == CI | `bupha run` is literally the same path | `act` approximates |
| batching | supersede built-in, honest "cancelled" statuses | `concurrency` groups |
| scale | one worker, serialized runs | matrix/fleet |
| cost | your box only | minutes billing |

**When GHA is better:** you need parallelism, matrices across OSes,
hosted macOS/Windows builds, or marketplace actions. **When bupha
fits:** instant feedback on self-owned hardware, secrets that
never leave the machine, warm caches on self-owned storage, no
YAML.

## vs others

* **Jenkins/Buildkite** — bupha is a single binary with a TOML file,
  not a server product to operate.
* **Woodpecker/Forgejo CI** — bupha is forge-webhook driven and much
  smaller; those are full pipeline platforms.
* **self-hosted GHA runner** — bupha is lighter (no runner service,
  no org config) but has no matrix or marketplace.
