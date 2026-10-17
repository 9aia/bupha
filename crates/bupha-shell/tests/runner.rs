
use bupha_core::RunnerCfg;
use bupha_runner::{RunCtx, Runner};
use bupha_shell::ShellRunner;
use std::io::Read;

#[test]
fn runs_cmd_in_worktree() {
    let r = ShellRunner::new(RunnerCfg {
        cmd: Some("pwd; echo sha:$BUPHA_SHA".into()), ..Default::default() });
    let mut c = r.spawn(&RunCtx { workdir: "/tmp".as_ref(),
        token_env: "BUPHA_TOKEN" }).unwrap();
    c.wait().map(|s| assert!(s.success())).unwrap();
}
