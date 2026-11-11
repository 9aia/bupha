+++
id = "P-003"
title = "bupha-runner port is load-bearing — keep; redundancy is queue/store (T-049) and worktree plumbing"
status = "open"
created = "2026-11-11"
priority = "med"
+++

Explore analysis (2026-11-11): bupha-runner is NOT redundant vs peregrin/hypha/tamias. Peregrin drivers are sync/blocking (600s cap, no mid-flight kill, no detached handle, no infra-vs-pipeline failure classification); hypha has zero exec; tamias distributes only; sepia is agent sessions. Runner port (67 lines) provides RunHandle {try_wait/wait/kill/failure_desc} — the seam enabling cancel-in-flight, lease-loss kill, unbounded jobs, oom-kill classification. Per-impl: shell/dagger/gha-dialect/ssh/container/http/gha-runner all KEEP (unique or canonical). Optional polish only: worktree plumbing overlaps peregrin-git — semantics differ enough to leave. ADR-0006 already assigns runner impls to bupha. Real redundancy was queue/store (T-049, done) not the runner.
