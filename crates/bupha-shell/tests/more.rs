//! ShellRunner coverage beyond runner.rs: name(), the ./ci.sh
//! default, and spawn failure.

use bupha_runner::{RunCtx, Runner};
use bupha_shell::{ShellCfg, ShellRunner};

#[test]
fn name_and_default_cmd() {
    let r = ShellRunner::new(ShellCfg { cmd: None });
    assert_eq!(r.name(), "script");
    // workdir without ci.sh -> bash -c ./ci.sh still spawns; exits 127
    let d = std::env::temp_dir().join(format!("bupha-sh-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    let mut c = r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
        })
        .unwrap();
    assert_eq!(c.wait().unwrap().code(), Some(127));
}

#[test]
fn spawn_fails_without_bash() {
    // An empty PATH dir makes `bash` unresolvable.
    let d = std::env::temp_dir().join(format!("bupha-sh2-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    std::env::set_var("PATH", &d);
    let r = ShellRunner::new(ShellCfg {
        cmd: Some("true".into()),
    });
    assert!(r
        .spawn(&RunCtx {
            workdir: &d,
            token_env: "T",
        })
        .is_err());
}
