//! SigstoreAttest with a stub `cosign` binary on PATH — covers arg
//! construction, the signature file landing, cosign failures, and
//! the not-on-PATH / non-NotFound spawn errors.

use bupha_attest::{Attest, RunAttestation};
use bupha_attest_sigstore::SigstoreAttest;
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-sig-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub(dir: &Path, name: &str, body: &str) -> PathBuf {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn att() -> RunAttestation {
    RunAttestation {
        sha: "0123456789abcdef".into(),
        repo: "org/repo".into(),
        runner: "shell".into(),
        status: "success".into(),
        wall_secs: 3,
    }
}

fn path_with(dir: &Path) {
    std::env::set_var(
        "PATH",
        format!(
            "{}:{}",
            dir.display(),
            std::env::var("PATH").unwrap_or_default()
        ),
    );
}

#[test]
fn signs_predicate_and_stores_sig() {
    let d = dir("ok");
    stub(
        &d,
        "cosign",
        &format!(
            "echo \"$@\" > \"{}/args\"\nwhile [ $# -gt 0 ]; do\n  if [ \"$1\" = \"--output-signature\" ]; then echo sig > \"$2\"; fi\n  shift\ndone\nexit 0",
            d.display()
        ),
    );
    path_with(&d);
    let a = SigstoreAttest {
        dir: d.join("att"),
        key: "env://MY_KEY".into(),
    };
    let sig = a.attest(&att()).unwrap();
    assert!(sig.exists());
    let args = std::fs::read_to_string(d.join("args")).unwrap();
    assert!(args.contains("sign-blob --yes"));
    assert!(args.contains("--key env://MY_KEY"));
    assert!(args.contains("org-repo-0123456789ab.json"));
}

#[test]
fn keyless_omits_key_arg() {
    let d = dir("kl");
    stub(
        &d,
        "cosign",
        &format!("echo \"$@\" > \"{}/args\"\nexit 0", d.display()),
    );
    path_with(&d);
    let a = SigstoreAttest {
        dir: d.join("att"),
        key: String::new(),
    };
    let _ = a.attest(&att());
    let args = std::fs::read_to_string(d.join("args")).unwrap();
    assert!(!args.contains("--key"));
}

#[test]
fn cosign_failure_errors() {
    let d = dir("fail");
    stub(&d, "cosign", "echo nope >&2\nexit 1");
    path_with(&d);
    let a = SigstoreAttest {
        dir: d.join("att"),
        key: String::new(),
    };
    let e = a.attest(&att()).unwrap_err();
    assert!(e.to_string().contains("cosign:"));
}

#[test]
fn missing_cosign_reports_install_hint() {
    let d = dir("none");
    std::env::set_var("PATH", &d);
    let a = SigstoreAttest {
        dir: d.join("att"),
        key: String::new(),
    };
    let e = a.attest(&att()).unwrap_err();
    assert!(e.to_string().contains("cosign not on PATH"));
}

#[test]
fn non_executable_cosign_propagates_error() {
    let d = dir("perm");
    // Exists but not executable -> PermissionDenied, not NotFound.
    let p = d.join("cosign");
    std::fs::write(&p, "#!/bin/bash\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o644)).unwrap();
        std::env::set_var("PATH", &d);
    }
    let a = SigstoreAttest {
        dir: d.join("att"),
        key: String::new(),
    };
    let e = a.attest(&att()).unwrap_err();
    assert!(!e.to_string().contains("not on PATH"));
}
