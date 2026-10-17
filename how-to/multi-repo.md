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
queue = "all"                    # every push runs (releases)

[[repo]]
name = "org/web"
dir = "/srv/checkouts/web"       # default latest-wins

# [repo.runner]                # per-repo override; replaces global
#   kind = "script"
#   cmd = "./release.sh"
```

Run `bupha deploy webhook` once — it registers the hook on every
watched repo. `status_repo` on an entry redirects statuses (e.g.
mirror repos); it defaults to `name`.
