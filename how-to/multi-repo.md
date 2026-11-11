# How-to: watch multiple repos

Add one `[[repo]]` entry per repository; `name` must equal the
payload's `repository.full_name`. Each repo gets its own pending
slot; a single global worker serializes runs (deliberate — safe on
one box). Same-repo pushes may cancel their own in-flight run
(`cancel_running`); a push never cancels another repo's run.

Snippets are TOML; the equivalent `bupha.yaml`/`bupha.yml` works
identically and is preferred when both exist (TOML is permanent —
see [config reference](../reference/config.md)).

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

## Multiple pipelines on one repo — `[[repo.jobs]]`

A repo that needs more than one pipeline (a main gate plus a
docs-branch deploy, a debounced release next to the CI gate)
declares jobs instead of a second `[[repo]]` — duplicate `name`
entries are rejected at load. Each job gets its own `branch`,
`queue`, `runner` and status context; branch/queue/runner fall back
job → repo → global. Jobs share the repo's webhook, `secret_env`
and clone — one push to a ref feeds every job watching it.

```toml
[[repo]]
name = "org/app"
dir = "/srv/checkouts/app"

[[repo.jobs]]
name = "gate"                    # worktrees: <base>-org-app-gate-<sha12>
branch = "refs/heads/main"       # statuses: ci/bupha/gate
[repo.jobs.runner]
kind = "shell"
cmd = "bash scripts/ci.sh"

[[repo.jobs]]
name = "docs"
branch = "refs/heads/docs"
context = "ci/docs-deploy"       # status context override
[repo.jobs.runner]
kind = "shell"
cmd = "bash scripts/deploy-docs.sh"
```

`bupha runs org/app` (dev CLI) lists `org/app:gate` and
`org/app:docs` records; `bupha runs org/app:gate` filters to the
one job. On a deployed bundle the same data comes from the
driver's `bupha:runs` pavo view (`repo` param matches
`owner/name` and `owner/name:job` keys).

`secret_env` names an env var (resolved like `trigger.secret_env` —
env → cuora (canonical — every fleet secret migrated, T-059) → keyring → `~/.config/bupha/env` (env file retired, legacy code fallback only),
never the value. Each repo's
hook can then be managed and rotated independently: the watcher
resolves the target repo before verifying, so a push signed with one
repo's secret fails for every other repo — knowing the global secret
(or another team's) doesn't let a hook admin cross-forge. Repos
without `secret_env` share the global.

Standalone/dev: run `bupha deploy webhook` once — it registers
the hook on every watched repo, each with its own secret.
Bundle path: `tamias driver exec <bundle> hooks register` does
the same from `provider.ingress[].hooks[]` — **standalone/dev
ingress** per ADR-0007 (production ingress is a peregrin
campaign). `status_repo` on an entry redirects statuses (e.g.
mirror repos); it defaults to `name`.
