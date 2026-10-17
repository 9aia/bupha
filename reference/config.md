---
url: /bupha/reference/config.md
---
# bupha.toml reference

Tables: `[[repos]]` (name, dir, branch, status_repo?, queue?) ·
`[trigger]` (kind, listen, secret_env) · `[runner]` (kind, function,
shards, jobs, dogfood_jobs, sequential, cancel_running, sccache_conf,
cmd, token_env, extra_args, cli_source) · `[status]` (kind, repo,
context) · `[tunnel]` (provider, name, local_url, public_url) ·
`[worktree]` (base).

Unknown fields error loudly (`deny_unknown_fields`). Secrets are env
vars named by `secret_env`/`token_env` — never written to the file.

Constraints the loader enforces:

* `queue = "all"` on a `[[repos]]` entry rejects
  `runner.cancel_running` — `all` promises every push runs; killing the
  in-flight job would silently drop a sha that was never superseded.
* With the `dagger` runner, the emitted `dagger call` args are
  `--shards --jobs --token` plus `dogfood_jobs`/`sequential`/
  `sccache_conf`/`extra_args` only when set — the module function's
  signature must declare everything the runner sends.
* The webhook `after` sha must be hex (7–64 chars); anything else gets
  a 400, and branch deletions (`after = 000…`) are ignored cleanly.
