---
url: /bupha/reference/cli.md
---
# Command-Line Help for `bupha`

This document contains the help content for the `bupha` command-line program.

**Command Overview:**

* [`bupha`↴](#bupha)
* [`bupha watch`↴](#bupha-watch)
* [`bupha init`↴](#bupha-init)
* [`bupha doctor`↴](#bupha-doctor)
* [`bupha run`↴](#bupha-run)
* [`bupha deploy`↴](#bupha-deploy)
* [`bupha deploy service`↴](#bupha-deploy-service)
* [`bupha deploy webhook`↴](#bupha-deploy-webhook)
* [`bupha deploy env`↴](#bupha-deploy-env)
* [`bupha deploy check`↴](#bupha-deploy-check)
* [`bupha deploy release`↴](#bupha-deploy-release)
* [`bupha destroy`↴](#bupha-destroy)
* [`bupha destroy service`↴](#bupha-destroy-service)
* [`bupha destroy webhook`↴](#bupha-destroy-webhook)
* [`bupha destroy worktrees`↴](#bupha-destroy-worktrees)

## `bupha`

the oxpecker on your repo's back — headless CI

**Usage:** `bupha [OPTIONS] <COMMAND>`

###### **Subcommands:**

* `watch` — Serve the webhook → queue → runner loop
* `init` — Scaffold CI in a repo (dagger module install or script skeleton)
* `doctor` — Check what the loaded config actually needs on this box
* `run` — Run the pipeline locally (the runner's invocation, current dir)
* `deploy` — Install-time helpers — no files to copy by hand
* `destroy` — Teardown counterparts of `deploy`

###### **Options:**

* `--config <CONFIG>` — Path to bupha.toml (default: ./bupha.toml or ~/.config/bupha/)

## `bupha watch`

Serve the webhook → queue → runner loop

**Usage:** `bupha watch`

## `bupha init`

Scaffold CI in a repo (dagger module install or script skeleton)

**Usage:** `bupha init [DIR]`

###### **Arguments:**

* `<DIR>` — Repo dir to initialize (default: cwd)

  Default value: `.`

## `bupha doctor`

Check what the loaded config actually needs on this box

**Usage:** `bupha doctor`

## `bupha run`

Run the pipeline locally (the runner's invocation, current dir)

**Usage:** `bupha run [FUNCTION]`

###### **Arguments:**

* `<FUNCTION>` — Function for the dagger runner (default: from config)

## `bupha deploy`

Install-time helpers — no files to copy by hand

**Usage:** `bupha deploy <COMMAND>`

###### **Subcommands:**

* `service` — Write + enable the systemd user unit (needs enable-linger for boot persistence)
* `webhook` — Register/update the repo's push webhook via `gh api`
* `env` — Seed ~/.config/bupha/env with the secret names
* `check` — Verify the whole install: unit, secrets, hook, endpoint, token
* `release` — One-time seed: cargo publish in dep order + tag {repo}-v{ver}. Explicit — publishing is the irreversible step in the flow

## `bupha deploy service`

Write + enable the systemd user unit (needs enable-linger for boot persistence)

**Usage:** `bupha deploy service [OPTIONS]`

###### **Options:**

* `--no-start` — Skip enable --now (write the file only)

## `bupha deploy webhook`

Register/update the repo's push webhook via `gh api`

**Usage:** `bupha deploy webhook`

## `bupha deploy env`

Seed ~/.config/bupha/env with the secret names

**Usage:** `bupha deploy env`

## `bupha deploy check`

Verify the whole install: unit, secrets, hook, endpoint, token

**Usage:** `bupha deploy check`

## `bupha deploy release`

One-time seed: cargo publish in dep order + tag {repo}-v{ver}. Explicit — publishing is the irreversible step in the flow

**Usage:** `bupha deploy release [OPTIONS]`

###### **Options:**

* `--dry-run` — Print the publish order and tag without doing anything

## `bupha destroy`

Teardown counterparts of `deploy`

**Usage:** `bupha destroy <COMMAND>`

###### **Subcommands:**

* `service` — Disable + remove the systemd user unit
* `webhook` — Delete the repo's webhook whose URL matches tunnel.public_url
* `worktrees` — Remove detached worktrees and the persisted queue file

## `bupha destroy service`

Disable + remove the systemd user unit

**Usage:** `bupha destroy service`

## `bupha destroy webhook`

Delete the repo's webhook whose URL matches tunnel.public_url

**Usage:** `bupha destroy webhook`

## `bupha destroy worktrees`

Remove detached worktrees and the persisted queue file

**Usage:** `bupha destroy worktrees`

&#xA;This document was generated automatically by
clap-markdown.&#xA;
