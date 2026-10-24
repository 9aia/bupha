//! KeyringSecrets with a stub `secret-tool` on PATH — covers lookup
//! hit/miss/empty, missing binary, store round-trip and store failure.

use bupha_keyring::KeyringSecrets;
use bupha_secret::Secrets;
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-key-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// secret-tool stub: `lookup` prints $STUB_LOOKUP (exit $STUB_LOOKUP_CODE),
/// `store` writes stdin to <dir>/stored and exits $STUB_STORE_CODE.
fn stub_secret_tool(dir: &Path) {
    let p = dir.join("secret-tool");
    std::fs::write(
        &p,
        format!(
            "#!/bin/bash\nif [ \"$1\" = lookup ]; then\n  [ -n \"$STUB_LOOKUP\" ] || exit 1\n  echo \"$STUB_LOOKUP\"\n  exit 0\nfi\nif [ \"$1\" = store ]; then\n  cat > \"{}/stored\"\n  exit ${{STUB_STORE_CODE:-0}}\nfi\nexit 2\n",
            dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
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
fn lookup_hit_miss_and_empty() {
    let d = dir("get");
    stub_secret_tool(&d);
    let k = KeyringSecrets::default();

    std::env::set_var("STUB_LOOKUP", "sekrit");
    assert_eq!(k.get("TOKEN").as_deref(), Some("sekrit"));

    std::env::set_var("STUB_LOOKUP", "");
    assert!(k.get("TOKEN").is_none(), "lookup exit!=0 -> miss");

    // Success with empty stdout is also a miss.
    let p = d.join("secret-tool");
    std::fs::write(&p, "#!/bin/bash\nexit 0\n").unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(k.get("TOKEN").is_none());
}

#[test]
fn missing_binary_is_a_miss_and_set_errors() {
    let d = dir("none");
    std::env::set_var("PATH", &d);
    let k = KeyringSecrets {
        service: "bupha".into(),
    };
    assert!(k.get("TOKEN").is_none());
    assert!(k.set("TOKEN", "v").is_err());
}

#[test]
fn store_writes_stdin_and_failure_errors() {
    let d = dir("set");
    stub_secret_tool(&d);
    let k = KeyringSecrets::default();

    std::env::set_var("STUB_STORE_CODE", "0");
    k.set("TOKEN", "sekrit").unwrap();
    assert_eq!(std::fs::read_to_string(d.join("stored")).unwrap(), "sekrit");

    std::env::set_var("STUB_STORE_CODE", "1");
    let e = k.set("TOKEN", "x").unwrap_err();
    assert!(e.to_string().contains("secret-tool store failed"));

    // child exits instantly without reading -> write_all hits EPIPE
    std::fs::write(d.join("secret-tool"), "#!/bin/sh\nexit 0\n").unwrap();
    assert!(k.set("TOKEN", &"v".repeat(200_000)).is_err());
}
