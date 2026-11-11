//! CuoraSecrets against a stub `cuora` binary — covers the ref
//! mapping, get hit/miss/missing-binary, newline stripping, and the
//! `put` stdin path. No PATH games: `CuoraSecrets.bin` points
//! straight at the stub.

#![allow(clippy::unwrap_used)]

use bupha_secret::Secrets;
use bupha_secret_cuora::{rref, CuoraSecrets};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// STUB_* env is process-global — serialize this file's tests.
static LOCK: Mutex<()> = Mutex::new(());

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-cuora-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// cuora stub: `--actor bupha get <ref>` prints $STUB_GET (exit
/// $STUB_GET_CODE); `put <ref>` stores stdin at <dir>/stored and
/// exits $STUB_PUT_CODE.
fn stub(dir: &Path) -> PathBuf {
    let p = dir.join("cuora");
    std::fs::write(
        &p,
        format!(
            "#!/bin/bash\n\
             if [ \"$1\" = \"--actor\" ]; then shift 2; fi\n\
             case \"$1\" in\n\
               get) if [ \"${{STUB_GET_SET:-0}}\" = 1 ]; then printf '%s\\n' \"$STUB_GET\"; exit ${{STUB_GET_CODE:-0}}; else exit 1; fi;;\n\
               put) cat > \"{}/stored\"; exit ${{STUB_PUT_CODE:-0}};;\n\
               *) exit 2;;\n\
             esac\n",
            dir.display()
        ),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    p
}

fn store_at(bin: PathBuf) -> CuoraSecrets {
    CuoraSecrets { bin }
}

#[test]
fn rref_maps_name_into_bupha_namespace() {
    let _g = LOCK.lock().unwrap();
    assert_eq!(
        rref("BUPHA_WH_9AIA_BUPHA"),
        "cuora://bupha/BUPHA_WH_9AIA_BUPHA"
    );
    assert_eq!(rref("BUPHA_TOKEN"), "cuora://bupha/BUPHA_TOKEN");
}

#[test]
fn get_returns_value_without_trailing_newline() {
    let _g = LOCK.lock().unwrap();
    let d = dir("get");
    let s = store_at(stub(&d));
    std::env::set_var("STUB_GET_SET", "1");
    std::env::set_var("STUB_GET", "whsec_abc123");
    assert_eq!(
        s.get("BUPHA_WH_9AIA_BUPHA").as_deref(),
        Some("whsec_abc123")
    );
    std::env::remove_var("STUB_GET_SET");
    std::env::remove_var("STUB_GET");
}

#[test]
fn get_miss_and_missing_binary_are_none_not_error() {
    let _g = LOCK.lock().unwrap();
    let d = dir("miss");
    let s = store_at(stub(&d));
    std::env::remove_var("STUB_GET_SET");
    assert_eq!(s.get("BUPHA_WH_9AIA_BUPHA"), None);

    let absent = store_at(d.join("no-such-cuora"));
    assert_eq!(absent.get("BUPHA_TOKEN"), None);
}

#[test]
fn get_empty_stdout_is_a_miss() {
    let _g = LOCK.lock().unwrap();
    let d = dir("empty");
    let s = store_at(stub(&d));
    std::env::set_var("STUB_GET_SET", "1");
    std::env::set_var("STUB_GET", "");
    // stub prints just "\n" — an empty value is no secret.
    assert_eq!(s.get("BUPHA_TOKEN"), None);
    std::env::remove_var("STUB_GET_SET");
    std::env::remove_var("STUB_GET");
}

#[test]
fn set_pipes_value_on_stdin_never_argv() {
    let _g = LOCK.lock().unwrap();
    let d = dir("set");
    let s = store_at(stub(&d));
    s.set("BUPHA_TOKEN", "tok_123\nwith newline").unwrap();
    assert_eq!(
        std::fs::read_to_string(d.join("stored")).unwrap(),
        "tok_123\nwith newline"
    );
}

#[test]
fn set_failure_is_an_io_error() {
    let _g = LOCK.lock().unwrap();
    let d = dir("setfail");
    let s = store_at(stub(&d));
    std::env::set_var("STUB_PUT_CODE", "1");
    assert!(s.set("BUPHA_TOKEN", "x").is_err());
    std::env::remove_var("STUB_PUT_CODE");
}
