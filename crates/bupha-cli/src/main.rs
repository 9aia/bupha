//! bupha — compat CLI driver. The engine is the `bupha` lib
//! (src/lib.rs — the clap tree, dispatch, and every verb); this
//! binary is just `bupha::run()` mapped to a process exit code.
//! `tamias-driver-bupha` reaches the same surface through the lib
//! (`ops` manifest + `run` passthrough) — see `bupha-driver`.

fn main() -> std::process::ExitCode {
    // Engine codes are 0/1/2 or a child's exit status — anything
    // outside u8 collapses to failure rather than wrapping.
    std::process::ExitCode::from(u8::try_from(bupha::run()).unwrap_or(1))
}
