//! ShellRunner coverage beyond runner.rs: name(), the ./ci.sh
//! default, and spawn failure.

use bupha_runner::{RunCtx, Runner};
use bupha_shell::{ShellCfg, ShellRunner};
use std::sync::Mutex;

/// Env (PATH) is process-global — serialize this file's tests so
/// the exclusive-PATH setup can't break the parallel bash spawn.
static LOCK: Mutex<()> = Mutex::new(());

/// Restore PATH at scope end.
struct PathGuard(Option<String>);
impl PathGuard {
    fn capture() -> Self {
        Self(std::env::var("PATH").ok())
    }
}
impl Drop for PathGuard {
    fn drop(&mut self) {
        if let Some(p) = &self.0 {
            std::env::set_var("PATH", p);
        }
    }
}

#[test]
fn name_and_default_cmd() {
    let _g = LOCK.lock().unwrap();
    let r = ShellRunner::new(ShellCfg {
        cmd: None,
        isolate: None,
        memory_max: None,
    });
    assert_eq!(r.name(), "script");
    // workdir without ci.sh -> bash -c ./ci.sh still spawns; exits 127
    let d = std::env::temp_dir().join(format!("bupha-sh-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
            key: "o/r",
            sha: "abc1234567890def",
        })
        .unwrap();
    assert_eq!(c.wait().unwrap().code(), Some(127));
}

#[test]
fn spawn_fails_without_bash() {
    let _g = LOCK.lock().unwrap();
    let _path = PathGuard::capture();
    // An empty PATH dir makes `bash` unresolvable.
    let d = std::env::temp_dir().join(format!("bupha-sh2-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::env::set_var("PATH", &d);
    let r = ShellRunner::new(ShellCfg {
        cmd: Some("true".into()),
        isolate: None,
        memory_max: None,
    });
    assert!(r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
            key: "o/r",
            sha: "abc1234567890def",
        })
        .is_err());
}
