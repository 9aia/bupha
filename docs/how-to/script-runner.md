# How-to: run without containers

`runner.kind = "script"` + `cmd = "./ci.sh"` — the watcher does
worktrees + queue + statuses; your script does the build. No dagger,
no docker. Useful for hosts where containers are unavailable or
unwanted.
