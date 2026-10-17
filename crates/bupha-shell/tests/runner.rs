use bupha_runner::{RunCtx, Runner};
use bupha_shell::{ShellCfg, ShellRunner};

#[test]
fn runs_cmd_in_worktree() {
    let r = ShellRunner::new(ShellCfg {
        cmd: Some("pwd".into()),
    });
    let mut c = r
        .spawn(&RunCtx {
            workdir: "/tmp".as_ref(),
            token_env: "BUPHA_TOKEN",
        })
        .unwrap();
    c.wait().map(|s| assert!(s.success())).unwrap();
}

#[test]
fn failure_propagates() {
    let r = ShellRunner::new(ShellCfg {
        cmd: Some("exit 3".into()),
    });
    let mut c = r
        .spawn(&RunCtx {
            workdir: "/tmp".as_ref(),
            token_env: "BUPHA_TOKEN",
        })
        .unwrap();
    c.wait().map(|s| assert!(!s.success())).unwrap();
}
