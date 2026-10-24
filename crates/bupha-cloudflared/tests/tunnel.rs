//! CloudflaredTunnel with a stub `cloudflared` on PATH — covers arg
//! construction (quick vs named tunnel), name, public_url, and the
//! spawned child handle.

use bupha_cloudflared::CloudflaredTunnel;
use bupha_core::TunnelCfg;
use bupha_tunnel::Tunnel;
use std::path::{Path, PathBuf};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-cf-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub_cloudflared(dir: &Path) {
    let p = dir.join("cloudflared");
    std::fs::write(
        &p,
        // `exec` so killing the child reaps the sleep too — a plain
        // `sleep` would orphan it (leaky test).
        format!(
            "#!/bin/bash\necho \"$@\" > \"{}/args\"\nexec sleep 60\n",
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

fn cfg(name: &str, public_url: &str) -> TunnelCfg {
    TunnelCfg {
        name: name.into(),
        local_url: "http://127.0.0.1:9000".into(),
        public_url: public_url.into(),
        ..Default::default()
    }
}

/// The stub writes its argv on start — poll until it lands (spawn
/// returns before the shell script gets scheduled).
fn args_of(d: &Path) -> String {
    for _ in 0..200 {
        if let Ok(s) = std::fs::read_to_string(d.join("args")) {
            return s;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("stub never wrote args file")
}

#[test]
fn quick_tunnel_args() {
    let d = dir("quick");
    stub_cloudflared(&d);
    let t = CloudflaredTunnel::new(cfg("", ""));
    assert_eq!(t.name(), "cloudflared");
    assert!(t.public_url().is_none());
    let mut c = t.spawn().unwrap().unwrap();
    let args = args_of(&d);
    assert!(args.contains("tunnel --url http://127.0.0.1:9000 --no-autoupdate"));
    let _ = c.kill();
    let _ = c.wait();
}

#[test]
fn named_tunnel_args_and_public_url() {
    let d = dir("named");
    stub_cloudflared(&d);
    let t = CloudflaredTunnel::new(cfg("my-tunnel", "https://ci.example.com"));
    assert_eq!(t.public_url().as_deref(), Some("https://ci.example.com"));
    let mut c = t.spawn().unwrap().unwrap();
    let args = args_of(&d);
    assert!(args.contains("tunnel run my-tunnel"));
    let _ = c.kill();
    let _ = c.wait();
}

#[test]
fn spawn_fails_without_binary() {
    let d = dir("nope");
    std::env::set_var("PATH", &d);
    let t = CloudflaredTunnel::new(cfg("", ""));
    assert!(t.spawn().is_err());
}
