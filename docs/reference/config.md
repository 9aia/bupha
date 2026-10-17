# bupha.toml reference

Tables: `[[repos]]` (name, dir, branch, status_repo?) · `[trigger]`
(kind, listen, secret_env) · `[runner]` (kind, function, shards, jobs,
sequential, cancel_running, sccache_conf, cmd, token_env, extra_args)
· `[status]` (kind, repo, context) · `[tunnel]` (provider, name,
local_url, public_url) · `[worktree]` (base).

Unknown fields error loudly (`deny_unknown_fields`). Secrets are env
vars named by `secret_env`/`token_env` — never written to the file.
