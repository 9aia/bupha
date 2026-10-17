# TODO — before 0.1.0

- [ ] `bupha deploy release` — publishes 9 crates in dep order +
      pushes `bupha-v0.1.0` (needs `CARGO_REGISTRY_TOKEN` from `.env`).
- [ ] After the tag: point `MODULE_SHA` at the tag name (or keep the
      sha — decide).
- [ ] Next push to `main`: watch `journalctl -u bupha-watch` for the
      first real `release.sh` run (debounce → publish touched crates
      → tag → dist binaries).
- [x] ~~Rebuild + restart service binary~~ — release build installed
      and `bupha-watch` restarted on `adbff4d`.

Next version: [project/TODO.md](project/TODO.md) ·
Horizon: [project/BACKLOG.md](project/BACKLOG.md)
