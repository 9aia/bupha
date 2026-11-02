+++
id = "I-001"
title = "CI jobs run in the watcher cgroup — OOM kills the watcher and re-runs forever"
status = "open"
created = "2026-11-02"
owner = "devin"
priority = "high"
labels = ["gotcha"]
+++

Observed 2026-11-02 on a16x: `handl-lang/graphopt` sha 6a220d68bc03 ran 12+ times. Its pytest lane peaked ~8.3GB anon-RSS on the 11GB box → kernel OOM-killed it → the kill landed inside `bupha-watch.service`'s cgroup → systemd `Restart=always` restarted the watcher → the same SHA was restored from the queue and re-ran → OOM again. ~15–40min of heavy work per cycle, all day.

Two structural problems:

1. **No job isolation / memory cap.** CI jobs share the watcher unit's cgroup, so a fat job can kill the watcher and freeze the desktop. Jobs should run under their own cgroup/slice with `MemoryMax` — then an over-limit job gets OOM'd alone, the watcher records a clean failure, and the host survives.

2. **Deterministic kills are retried as if transient.** A SIGKILL/cgroup-OOM can never pass on retry (same sha, same memory). The runner should classify oom/SIGKILL deaths as permanent (or cap retries) instead of re-queueing indefinitely. Same class as the ~455s-uniform `9aia/bupha` failures (probable timeout) and the sub-3s `myco:*` fast-fails — all currently opaque `failure` records.

Mitigation applied config-side meanwhile: `TORCHINDUCTOR_COMPILE_THREADS=1` on the graphopt runner cmd (`~/.config/bupha/bupha.toml`) so the inductor pool fits the box.
