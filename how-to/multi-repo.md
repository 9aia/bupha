---
url: /bupha/how-to/multi-repo.md
---
# How-to: watch multiple repos

Add one `[[repo]]` entry per repository; `name` must equal the
payload's `repository.full_name`. Each repo gets its own pending
slot; a single global worker serializes runs (deliberate — safe on
one box). Same-repo pushes may cancel their own in-flight run
(`cancel_running`); a push never cancels another repo's run.

```toml
[[repo]]
name = "org/api"
dir = "/srv/checkouts/api"
queue = "all" # every push runs (releases)
secret_env = "API_WEBHOOK_SECRET" # optional per-repo hook secret

[[repo]]
name = "org/web"
dir = "/srv/checkouts/web" # default latest-wins (and the global
                           # trigger.secret_env)

# [repo.runner]                # per-repo override; replaces global
#   kind = "shell"
#   cmd = "./release.sh"
```

`secret_env` names an env var (resolved like `trigger.secret_env` —
env → keyring → `~/.config/bupha/env`), never the value. Each repo's
hook can then be managed and rotated independently: the watcher
resolves the target repo before verifying, so a push signed with one
repo's secret fails for every other repo — knowing the global secret
(or another team's) doesn't let a hook admin cross-forge. Repos
without `secret_env` share the global.

Run `bupha deploy webhook` once — it registers the hook on every
watched repo, each with its own secret. `status_repo` on an entry
redirects statuses (e.g. mirror repos); it defaults to `name`.
