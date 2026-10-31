//! CLI subcommand coverage — every `bupha` command driven as a
//! spawned binary (exit statuses + stdout/stderr), with stub tools
//! (systemctl/journalctl/dagger/rsync/ssh/podman/cargo/git) on PATH
//! and stub HTTP forges where the CLI talks REST.

use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_redb::RedbQueueStore;
use std::collections::{BTreeMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn tmp(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("bupha-cli-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn stub(dir: &Path, name: &str, body: &str) {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/bash\n{body}\n")).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
}

fn bupha(args: &[&str], envs: &[(&str, String)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bupha"));
    c.args(args);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn bupha_in(args: &[&str], cwd: &Path, envs: &[(&str, String)]) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_bupha"));
    c.args(args).current_dir(cwd);
    for (k, v) in envs {
        c.env(k, v);
    }
    c.output().unwrap()
}

fn out_ok(o: &Output) -> String {
    format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    )
}

fn stubbed_path(dir: &Path) -> String {
    format!(
        "{}:{}",
        dir.display(),
        std::env::var("PATH").unwrap_or_default()
    )
}

/// Minimal forge API: handler sees "METHOD /path", returns (code, body).
fn serve(f: impl Fn(&str, &[u8]) -> (u16, String) + Send + 'static) -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(mut s) = stream else { continue };
            let mut r = BufReader::new(s.try_clone().unwrap());
            let mut reqline = String::new();
            if r.read_line(&mut reqline).is_err() {
                continue;
            }
            let mut clen = 0usize;
            loop {
                let mut h = String::new();
                if r.read_line(&mut h).is_err() || h.trim().is_empty() {
                    break;
                }
                if let Some((k, v)) = h.split_once(':') {
                    if k.trim().eq_ignore_ascii_case("content-length") {
                        clen = v.trim().parse().unwrap_or(0);
                    }
                }
            }
            let mut body = vec![0u8; clen];
            let _ = r.read_exact(&mut body);
            let (code, resp) = f(&reqline, &body);
            let _ = write!(
                s,
                "HTTP/1.1 {code} X\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{resp}",
                resp.len()
            );
        }
    });
    format!("http://127.0.0.1:{port}")
}

fn repo(dir: &Path) -> String {
    std::fs::create_dir_all(dir).unwrap();
    let g = |a: &[&str]| {
        Command::new("git")
            .args(a)
            .current_dir(dir)
            .output()
            .unwrap();
    };
    g(&["init", "-q", "-b", "main"]);
    g(&["config", "user.email", "t@t"]);
    g(&["config", "user.name", "t"]);
    std::fs::write(dir.join("f"), "x").unwrap();
    g(&["add", "."]);
    g(&["commit", "-qm", "c"]);
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .output()
        .unwrap();
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Baseline config used by most command tests.
fn cfg(dir: &Path, extra: &str) -> PathBuf {
    let p = dir.join("bupha.toml");
    std::fs::write(
        &p,
        format!(
            "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"true\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"none\"\n[worktree]\nbase = \"{}\"\n{extra}",
            dir.join("wt").display()
        ),
    )
    .unwrap();
    p
}

fn cfg_repo(dir: &Path, repo_dir: &Path, extra: &str) -> PathBuf {
    let p = dir.join("bupha.toml");
    std::fs::write(
        &p,
        format!(
            "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"true\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"none\"\n[worktree]\nbase = \"{}\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n{extra}",
            dir.join("wt").display(),
            repo_dir.display()
        ),
    )
    .unwrap();
    p
}

#[test]
fn gen_cli_docs() {
    let o = bupha(&["gen-cli-docs"], &[]);
    assert!(o.status.success());
    assert!(String::from_utf8_lossy(&o.stdout).contains("bupha watch"));
}

#[test]
fn config_errors_exit_2() {
    let t = tmp("cfgerr");
    // missing file
    let o = bupha(&["doctor", "--config", "/nonexistent/x.toml"], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("config:"));
    // malformed file
    let p = t.join("bad.toml");
    std::fs::write(&p, "[[[").unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &[]);
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("parse"));
}

#[test]
fn doctor_variants() {
    let t = tmp("doc");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let home = t.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let env: Vec<(&str, String)> = vec![
        ("HOME", home.display().to_string()),
        ("BUPHA_WEBHOOK_SECRET", "s".into()),
        ("BUPHA_TOKEN", "t".into()),
    ];
    // worktree.base exists so `df -m` yields a parseable free column
    // (the disk check block only runs when the path resolves).
    std::fs::create_dir_all(t.join("wt")).unwrap();
    let cfgp = cfg_repo(&t, &repo_dir, "");
    let o = bupha(&["doctor", "--config", cfgp.to_str().unwrap()], &env);
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("all green"));
    assert!(out_ok(&o).contains("disk"), "{}", out_ok(&o));

    // repo dir missing + no secrets -> failures -> exit 1
    let home2 = t.join("h2");
    std::fs::create_dir_all(&home2).unwrap();
    let cfgp2 = cfg_repo(&t, &t.join("nope"), "secret_env = \"REPO_WH\"\n");
    let o = bupha(
        &["doctor", "--config", cfgp2.to_str().unwrap()],
        &[
            ("HOME", home2.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("failure(s)"));

    // secret resolvable only via env file -> has() exercises file lines
    let cfgd = home2.join(".config/bupha");
    std::fs::create_dir_all(&cfgd).unwrap();
    std::fs::write(
        cfgd.join("env"),
        "BUPHA_WEBHOOK_SECRET=s3\nBUPHA_TOKEN=t\nREPO_WH=r\n",
    )
    .unwrap();
    let o = bupha(
        &["doctor", "--config", cfgp2.to_str().unwrap()],
        &[
            ("HOME", home2.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
        ],
    );
    assert!(out_ok(&o).contains("webhook secret") && out_ok(&o).contains("REPO_WH"));

    // status= github + no repos -> status.repo failure arm
    let p = t.join("gh.toml");
    std::fs::write(
        &p,
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"github\"\n",
    )
    .unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("no [[repo]] entries configured"));
    assert_eq!(o.status.code(), Some(1));

    // PATH without `df` -> the disk-check if-let takes its Err path.
    // kind=bogus keeps every earlier tool probe silent.
    let empty = t.join("empty-bin");
    std::fs::create_dir_all(&empty).unwrap();
    let p5 = t.join("df.toml");
    std::fs::write(
        &p5,
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"bogus\"\n[status]\nkind = \"log\"\n",
    )
    .unwrap();
    let o = bupha(
        &["doctor", "--config", p5.to_str().unwrap()],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", empty.display().to_string()),
            ("BUPHA_WEBHOOK_SECRET", "s".into()),
            ("BUPHA_TOKEN", "t".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("failure(s)"));
}

#[test]
fn doctor_runner_kinds_and_tools() {
    let t = tmp("dock");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "dagger", "echo dagger-version\nexit 0");
    stub(&bin, "docker", "echo docker-version\nexit 0");
    stub(&bin, "rsync", "--version\nexit 0");
    stub(&bin, "ssh", "exit 0");
    stub(&bin, "cloudflared", "echo cf\nexit 0");
    let env = vec![
        ("HOME", t.join("h").display().to_string()),
        ("E2E_SECRET", "s".into()),
        ("BUPHA_TOKEN", "t".into()),
        ("PATH", stubbed_path(&bin)),
    ];

    // dagger kind -> dagger + docker checks
    let p = t.join("d.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"dagger\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"cloudflared\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("dagger cli") && out_ok(&o).contains("docker"));

    // ssh kind -> rsync + ssh checks
    let p = t.join("s.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"ssh\"\nremote = \"u@h\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"nope-tunnel\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("rsync") && out_ok(&o).contains("unknown provider"));

    // unknown runner kind
    let p = t.join("u.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"bogus\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("unknown 'bogus'"));

    // sccache conf check: existing file ok + missing file fails
    let scc = t.join("scc.conf");
    std::fs::write(&scc, "x").unwrap();
    let p = t.join("c.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"true\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\n[cache]\nkind = \"sccache\"\nconf = \"{}\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        scc.display(), repo_dir.display()
    )).unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("sccache conf"));

    // legacy runner.sccache_conf (via runner.extra) also feeds doctor
    let p = t.join("c2.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"true\"\nsccache_conf = \"/missing/scc.conf\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["doctor", "--config", p.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("/missing/scc.conf"));
}

#[test]
fn run_matrix() {
    let t = tmp("run");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "dagger", "echo \"$@\" >> \"$DARGS\"\nexit 0");
    stub(&bin, "rsync", "exit 0");
    stub(&bin, "ssh", "exit 0");
    stub(&bin, "podman", "exit 0");
    let env = vec![("PATH", stubbed_path(&bin))];

    // shell success + function override + explicit repo runner kind
    let p = cfg_repo(&t, &repo_dir, "");
    let o = bupha(&["run", "--config", p.to_str().unwrap()], &env);
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("run passed"));

    let o = bupha(&["run", "ci2", "--config", p.to_str().unwrap()], &env);
    assert!(o.status.success(), "{}", out_ok(&o));

    // shell failure propagates exit code
    let p2 = t.join("f.toml");
    std::fs::write(&p2, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"exit 3\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["run", "--config", p2.to_str().unwrap()], &env);
    assert_eq!(o.status.code(), Some(3));

    // signal death -> exit 1 (no code)
    let p3 = t.join("k.toml");
    std::fs::write(&p3, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\ncmd = \"kill -9 $$\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["run", "--config", p3.to_str().unwrap()], &env);
    assert_eq!(o.status.code(), Some(1));

    // dagger + cache conf injection ([cache] wins into runner extra)
    std::env::set_var("DARGS", t.join("dargs"));
    let p4 = t.join("dg.toml");
    std::fs::write(&p4, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"dagger\"\ncli_source = \"/sib\"\n[status]\nkind = \"log\"\n[cache]\nkind = \"sccache\"\nconf = \"/srv/sccache.conf\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["run", "--config", p4.to_str().unwrap()],
        &[
            ("PATH", stubbed_path(&bin)),
            ("DARGS", t.join("dargs").display().to_string()),
        ][..],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    let args = std::fs::read_to_string(t.join("dargs")).unwrap();
    assert!(args.contains("--sccache-conf file:///srv/sccache.conf"));

    // legacy runner.sccache_conf moves into cache conf
    let p5 = t.join("dg2.toml");
    std::fs::write(&p5, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"dagger\"\nsccache_conf = \"/legacy/scc.conf\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let _ = std::fs::remove_file(t.join("dargs"));
    let o = bupha(
        &["run", "--config", p5.to_str().unwrap()],
        &[
            ("PATH", stubbed_path(&bin)),
            ("DARGS", t.join("dargs").display().to_string()),
        ][..],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(std::fs::read_to_string(t.join("dargs"))
        .unwrap()
        .contains("file:///legacy/scc.conf"));

    // cache kind none -> no injection
    let p6 = t.join("dg3.toml");
    std::fs::write(&p6, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"dagger\"\n[status]\nkind = \"log\"\n[cache]\nkind = \"none\"\nconf = \"/x\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let _ = std::fs::remove_file(t.join("dargs"));
    let o = bupha(
        &["run", "--config", p6.to_str().unwrap()],
        &[
            ("PATH", stubbed_path(&bin)),
            ("DARGS", t.join("dargs").display().to_string()),
        ][..],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(!std::fs::read_to_string(t.join("dargs"))
        .unwrap()
        .contains("sccache-conf"));

    // ssh runner
    let p7 = t.join("ssh.toml");
    std::fs::write(&p7, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"ssh\"\nremote = \"u@h\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["run", "--config", p7.to_str().unwrap()], &env);
    assert!(o.status.success(), "{}", out_ok(&o));

    // container runner
    let p8 = t.join("ct.toml");
    std::fs::write(&p8, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"container\"\nimage = \"img\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["run", "--config", p8.to_str().unwrap()], &env);
    assert!(o.status.success(), "{}", out_ok(&o));

    // container spawn failure (engine missing) -> "spawn failed"
    let p9 = t.join("ct2.toml");
    std::fs::write(&p9, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"container\"\nimage = \"img\"\nengine = \"no-such-engine\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["run", "--config", p9.to_str().unwrap()], &env);
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("spawn failed"));

    // unknown runner kind -> exit 2
    let p10 = t.join("u.toml");
    std::fs::write(&p10, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"bogus\"\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(&["run", "--config", p10.to_str().unwrap()], &env);
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("unknown runner kind"));

    // bad impl config per kind -> exit 2 arms
    for (kind, bad) in [
        ("dagger", "shards = 5"),
        ("shell", "cmd = 5"),
        ("ssh", "remote = 5"),
        ("container", "image = 5"),
    ] {
        let p = t.join(format!("bad-{kind}.toml"));
        std::fs::write(&p, format!(
            "[trigger]\nkind = \"github\"\n[runner]\nkind = \"{kind}\"\n{bad}\n[status]\nkind = \"log\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
            repo_dir.display()
        )).unwrap();
        let o = bupha(&["run", "--config", p.to_str().unwrap()], &env);
        assert_eq!(o.status.code(), Some(2), "{kind}: {}", out_ok(&o));
        assert!(out_ok(&o).contains(&format!("runner config ({kind})")));
    }

    // no repo configured -> exit 2
    let p = t.join("norepo.toml");
    std::fs::write(
        &p,
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n",
    )
    .unwrap();
    let o = bupha(&["run", "--config", p.to_str().unwrap()], &env);
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("no repo configured"));
}

#[test]
fn service_subcommands() {
    let t = tmp("svc");
    let home = t.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "systemctl", &format!(
        "echo \"$@\" >> \"{}/calls\"\nif [ \"$2\" = is-enabled ]; then echo enabled; fi\nif [ \"$2\" = is-active ]; then echo inactive; fi\nexit 0",
        t.display()
    ));
    stub(&bin, "journalctl", "echo journal-line\nexit 0");
    let env = vec![
        ("HOME", home.display().to_string()),
        ("PATH", stubbed_path(&bin)),
    ];

    for (args, want) in [
        (vec!["service", "enable"], "bupha-watch.service enabled"),
        (vec!["service", "disable"], "bupha-watch.service disabled"),
        (vec!["service", "status"], "enabled=enabled active=inactive"),
        (vec!["service", "restart"], "bupha-watch.service restarted"),
        (vec!["service", "logs"], "journal-line"),
        (vec!["service", "--worker", "enable"], "bupha-worker.service enabled"),
        (vec!["service", "--worker", "disable"], "bupha-worker.service disabled"),
        (vec!["service", "--worker", "restart"], "bupha-worker.service restarted"),
    ] {
        let o = bupha(&args, &env);
        assert!(o.status.success(), "{args:?}: {}", out_ok(&o));
        assert!(out_ok(&o).contains(want), "{args:?}: {}", out_ok(&o));
    }

    // follow mode returns journalctl's code; Ok(0) path too
    stub(&bin, "journalctl", "exit 7");
    let o = bupha(&["service", "logs", "-f"], &env);
    assert_eq!(o.status.code(), Some(7));
    stub(&bin, "journalctl", "exit 0");
    let o = bupha(&["service", "logs", "-f"], &env);
    assert!(o.status.success());

    // systemctl failure -> exit 1
    stub(&bin, "systemctl", "exit 1");
    let o = bupha(&["service", "enable"], &env);
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("service:"));

    // journalctl missing entirely -> service_logs spawn Err ->
    // `Err(e) => Err(e)` arm -> "service: ..." exit 1
    let empty = t.join("empty-bin");
    std::fs::create_dir_all(&empty).unwrap();
    let o = bupha(
        &["service", "logs"],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", empty.display().to_string()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("service:"));
}

#[test]
fn deploy_env_variants() {
    let t = tmp("denv");
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "secret-tool", "cat >/dev/null\nexit 0");
    let home = t.join("home");
    let cfgp = cfg(&t, "");

    // env var set -> keyring store; empty -> skipped
    let o = bupha(
        &["deploy", "env", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home.display().to_string()),
            ("XDG_CONFIG_HOME", t.join("xdg").display().to_string()),
            ("PATH", stubbed_path(&bin)),
            ("BUPHA_WEBHOOK_SECRET", "sekrit".into()),
            ("BUPHA_TOKEN", "".into()),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("stored in keyring"));
    // XDG_CONFIG_HOME beats HOME/.config for the env file location.
    assert!(t.join("xdg/bupha/env").exists());

    // per-repo secret_env names are seeded into the env file and
    // migrated to the keyring like the global
    let home4 = t.join("home4");
    let cfg4 = t.join("persec.toml");
    std::fs::write(&cfg4,
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"none\"\n[[repo]]\nname = \"o/r\"\ndir = \"/tmp\"\nsecret_env = \"REPO_WH\"\n"
    ).unwrap();
    let o = bupha(
        &["deploy", "env", "--config", cfg4.to_str().unwrap()],
        &[
            ("HOME", home4.display().to_string()),
            ("PATH", stubbed_path(&bin)),
            ("REPO_WH", "rs".into()),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("REPO_WH: stored in keyring"));
    let env_body = std::fs::read_to_string(home4.join(".config/bupha/env")).unwrap();
    assert!(env_body.contains("REPO_WH=") && env_body.contains("BUPHA_WEBHOOK_SECRET="));

    // no secret-tool -> keyring unavailable note
    let home2 = t.join("home2");
    let o = bupha(
        &["deploy", "env", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home2.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
            ("BUPHA_TOKEN", "tok".into()),
            ("BUPHA_WEBHOOK_SECRET", "sek".into()),
        ],
    );
    assert!(out_ok(&o).contains("keyring unavailable"));

    // env skeleton error: .config/bupha exists as a file
    let home3 = t.join("home3");
    std::fs::create_dir_all(home3.join(".config")).unwrap();
    std::fs::write(home3.join(".config/bupha"), "x").unwrap();
    let o = bupha(
        &["deploy", "env", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home3.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));

    // no HOME/XDG at all -> cfg_dir() falls back to ./.config
    let cwd = t.join("cwd");
    std::fs::create_dir_all(&cwd).unwrap();
    let o = Command::new(env!("CARGO_BIN_EXE_bupha"))
        .args(["deploy", "env", "--config"])
        .arg(&cfgp)
        .current_dir(&cwd)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("HOME")
        .env_remove("XDG_CONFIG_HOME")
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(
        cwd.join(".config/bupha/env").exists(),
        "fell back to ./.config"
    );
}

#[test]
fn deploy_service_variants() {
    let t = tmp("dsvc");
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "systemctl", "exit 0");
    let cfgp = cfg(&t, "");

    // --no-start writes the unit only
    let home = t.join("h1");
    let o = bupha(
        &[
            "deploy",
            "service",
            "--no-start",
            "--config",
            cfgp.to_str().unwrap(),
        ],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    let unit = home.join(".config/systemd/user/bupha-watch.service");
    assert!(unit.exists());
    assert!(std::fs::read_to_string(&unit).unwrap().contains("watch"));

    // --worker writes bupha-worker.service with the worker verb
    let homew = t.join("hw");
    let o = bupha(
        &[
            "deploy", "service", "--worker", "--no-start", "--config",
            cfgp.to_str().unwrap(),
        ],
        &[
            ("HOME", homew.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    let wunit = homew.join(".config/systemd/user/bupha-worker.service");
    let u = std::fs::read_to_string(&wunit).unwrap();
    assert!(u.contains(" worker --config") || u.contains(" worker
") || u.contains("worker"), "{u}");

    // default: write + enable --now
    let home2 = t.join("h2");
    let o = bupha(
        &["deploy", "service", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home2.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(out_ok(&o).contains("enabled"));

    // unit write fails -> exit 1
    let home3 = t.join("h3");
    std::fs::create_dir_all(home3.join(".config")).unwrap();
    std::fs::write(home3.join(".config/systemd"), "x").unwrap();
    let o = bupha(
        &["deploy", "service", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home3.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert_eq!(o.status.code(), Some(1));

    // enable fails -> exit 1
    stub(&bin, "systemctl", "exit 1");
    let home4 = t.join("h4");
    let o = bupha(
        &["deploy", "service", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home4.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
}

#[test]
fn deploy_webhook_variants() {
    let t = tmp("dwh");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let cfgp = cfg_repo(&t, &repo_dir, "");
    let env_base = vec![
        ("HOME", t.join("h").display().to_string()),
        ("BUPHA_WEBHOOK_SECRET", "wh".into()),
        ("BUPHA_TOKEN", "tok".into()),
    ];

    // no public_url -> exit 1
    let o = bupha(
        &["deploy", "webhook", "--config", cfgp.to_str().unwrap()],
        &env_base,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("public_url"));

    // secret missing -> exit 1 (first failure point)
    let p = t.join("np.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://x\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["deploy", "webhook", "--config", p.to_str().unwrap()],
        &[
            ("HOME", t.join("h").display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));

    // success against fake forge API
    let api = serve(|req, _| {
        if req.starts_with("GET") {
            (200, "[]".into())
        } else {
            (201, "{}".into())
        }
    });
    let p2 = t.join("ok.toml");
    std::fs::write(&p2, format!(
        "[trigger]\nkind = \"github\"\napi_base = \"{api}\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["deploy", "webhook", "--config", p2.to_str().unwrap()],
        &env_base,
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("created hook"));

    // forge error -> exit 1
    let api2 = serve(|_, _| (401, "{}".into()));
    let p3 = t.join("err.toml");
    std::fs::write(&p3, format!(
        "[trigger]\nkind = \"github\"\napi_base = \"{api2}\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["deploy", "webhook", "--config", p3.to_str().unwrap()],
        &env_base,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("webhook o/r"));

    // per-repo secret_env: o/r2 registers with its own secret
    let got = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    let g = got.clone();
    let api3 = serve(move |req, body| {
        if req.starts_with("GET") {
            return (200, "[]".into());
        }
        g.lock()
            .unwrap()
            .push(format!("{req} {}", String::from_utf8_lossy(body)));
        (201, "{}".into())
    });
    let p5 = t.join("persec.toml");
    std::fs::write(&p5, format!(
        "[trigger]\nkind = \"github\"\napi_base = \"{api3}\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n[[repo]]\nname = \"o/r\"\ndir = \"{d}\"\n[[repo]]\nname = \"o/r2\"\ndir = \"{d}\"\nsecret_env = \"REPO_WH\"\n",
        d = repo_dir.display()
    )).unwrap();
    // REPO_WH unset -> the per-repo resolution fails for o/r2
    let o = bupha(
        &["deploy", "webhook", "--config", p5.to_str().unwrap()],
        &env_base,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("REPO_WH"));
    let mut env2 = env_base.clone();
    env2.push(("REPO_WH", "repo-secret".into()));
    let o = bupha(
        &["deploy", "webhook", "--config", p5.to_str().unwrap()],
        &env2,
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    let seen = got.lock().unwrap();
    let r2 = seen.iter().find(|l| l.contains("/repos/o/r2/")).unwrap();
    assert!(r2.contains("\"repo-secret\""), "{r2}");
    let r1 = seen.iter().find(|l| l.contains("/repos/o/r/")).unwrap();
    assert!(r1.contains("\"wh\""), "{r1}");

    // public_url set but no [[repo]] -> watched() error arm
    let p4 = t.join("norepo.toml");
    std::fs::write(
        &p4,
        "[trigger]\nkind = \"github\"\nsecret_env = \"BUPHA_WEBHOOK_SECRET\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n",
    )
    .unwrap();
    let o = bupha(
        &["deploy", "webhook", "--config", p4.to_str().unwrap()],
        &env_base,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("[[repo]]"));
    let o = bupha(
        &["destroy", "webhook", "--config", p4.to_str().unwrap()],
        &env_base,
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("[[repo]]"));
}

#[test]
fn deploy_webhook_dedups_multi_job_repo() {
    // A [[repo]] with [[repo.jobs]] registers its hook ONCE — jobs
    // share the webhook, the secret and the clone.
    let t = tmp("dwhj");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let posts = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let p = posts.clone();
    let api = serve(move |req, _| {
        if req.starts_with("GET") {
            (200, "[]".into())
        } else {
            p.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            (201, "{}".into())
        }
    });
    let cfgp = t.join("jobs.toml");
    std::fs::write(&cfgp, format!(
        "[trigger]\nkind = \"github\"\napi_base = \"{api}\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n[[repo]]\nname = \"o/r\"\ndir = \"{d}\"\n[[repo.jobs]]\nname = \"gate\"\n[[repo.jobs]]\nname = \"docs\"\n",
        d = repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["deploy", "webhook", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", t.join("h").display().to_string()),
            ("BUPHA_WEBHOOK_SECRET", "wh".into()),
            ("BUPHA_TOKEN", "tok".into()),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert_eq!(
        posts.load(std::sync::atomic::Ordering::SeqCst),
        1,
        "one hook per repo, not per job"
    );
}

#[test]
fn deploy_check_variants() {
    let t = tmp("dchk");
    let cfgp = cfg(&t, "");

    // all green: unit + env file present, secrets in env, endpoint ok
    let api = serve(|_, _| (200, "{}".into()));
    let home = t.join("h1");
    std::fs::create_dir_all(home.join(".config/systemd/user")).unwrap();
    std::fs::write(home.join(".config/systemd/user/bupha-watch.service"), "x").unwrap();
    std::fs::create_dir_all(home.join(".config/bupha")).unwrap();
    std::fs::write(home.join(".config/bupha/env"), "E2E_SECRET=s\n").unwrap();
    let p = t.join("ok.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\nsecret_env = \"E2E_SECRET\"\n[runner]\nkind = \"shell\"\ntoken_env = \"MYTOK\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"{api}\"\n[[repo]]\nname = \"o/r\"\ndir = \"/tmp\"\nsecret_env = \"REPO_WH\"\n"
    )).unwrap();
    let o = bupha(
        &["deploy", "check", "--config", p.to_str().unwrap()],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
            ("MYTOK", "t".into()),
            ("REPO_WH", "rs".into()),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("all green"));

    // endpoint 404 still counts as reachable (StatusCode arm)
    let api404 = serve(|_, _| (404, "{}".into()));
    let p = t.join("404.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\nsecret_env = \"E2E_SECRET\"\n[runner]\nkind = \"shell\"\ntoken_env = \"MYTOK\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"{api404}\"\n"
    )).unwrap();
    let o = bupha(
        &["deploy", "check", "--config", p.to_str().unwrap()],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
            ("MYTOK", "t".into()),
        ],
    );
    assert!(out_ok(&o).contains("endpoint reachable"));

    // env file present but secret names only there EMPTY -> secret()
    // misses and the env-file line-scan closures run.
    let home9 = t.join("h9");
    std::fs::create_dir_all(home9.join(".config/bupha")).unwrap();
    std::fs::write(
        home9.join(".config/bupha/env"),
        "ZZ_MISSING_SECRET=\nZZ_MISSING_TOK=\n",
    )
    .unwrap();
    let p9 = t.join("scan.toml");
    std::fs::write(&p9, "[trigger]\nkind = \"github\"\nsecret_env = \"ZZ_MISSING_SECRET\"\n[runner]\nkind = \"shell\"\ntoken_env = \"ZZ_MISSING_TOK\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://x\"\n[[repo]]\nname = \"o/r\"\ndir = \"/tmp\"\nsecret_env = \"ZZ_REPO_WH\"\n").unwrap();
    let o = bupha(
        &["deploy", "check", "--config", p9.to_str().unwrap()],
        &[
            ("HOME", home9.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
        ],
    );
    assert!(
        out_ok(&o).contains("webhook secret")
            && out_ok(&o).contains("token set")
            && out_ok(&o).contains("ZZ_REPO_WH")
    );

    // unreachable + trycloudflare note -> failure exit
    let p = t.join("cf.toml");
    std::fs::write(&p,
        "[trigger]\nkind = \"github\"\nsecret_env = \"E2E_SECRET\"\n[runner]\nkind = \"shell\"\ntoken_env = \"MYTOK\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"https://x.trycloudflare.com\"\n"
    ).unwrap();
    let o = bupha(
        &["deploy", "check", "--config", p.to_str().unwrap()],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
            ("MYTOK", "t".into()),
        ],
    );
    assert!(out_ok(&o).contains("trycloudflare"));

    // nothing set up -> failures -> exit 1
    let o = bupha(
        &["deploy", "check", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", t.join("empty-home").display().to_string()),
            ("PATH", "/usr/bin:/bin".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("problem(s)"));
}

#[test]
fn deploy_release_variants() {
    let t = tmp("rel");
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    // cargo stub: metadata -> fixture; publish -> "Uploaded"
    let fixture = t.join("meta.json");
    std::fs::write(
        &fixture,
        r#"{"workspace_members":["a","b"],"packages":[
        {"name":"a","id":"a","version":"0.1.0","dependencies":[]},
        {"name":"b","id":"b","version":"0.1.0","dependencies":[{"name":"a","path":"crates/a"}]}
    ]}"#,
    )
    .unwrap();
    stub(&bin, "cargo", &format!(
        "case \"$*\" in\n *metadata*) cat \"{}\";;\n *publish*) echo \"Uploaded a-0.1.0\";;\nesac\nexit 0",
        fixture.display()
    ));
    stub(
        &bin,
        "git",
        "case \"$*\" in\n *rev-parse*verify*) exit 1;;\n *) exit 0;;\nesac",
    );

    // dry-run doesn't need the token — but covers order + tag print
    let cwd = t.join("proj");
    std::fs::create_dir_all(&cwd).unwrap();
    let o = bupha_in(
        &["deploy", "release", "--dry-run"],
        &cwd,
        &[("PATH", stubbed_path(&bin))],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("publish order: a -> b"));
    assert!(out_ok(&o).contains("tag: "));

    // real run: publishes (Uploaded) + tags + pushes via stubs
    let o = bupha_in(
        &["deploy", "release"],
        &cwd,
        &[
            ("PATH", stubbed_path(&bin)),
            ("CARGO_REGISTRY_TOKEN", "t".into()),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("published a") && out_ok(&o).contains("released"));

    // token missing -> exit 1
    let o = bupha_in(
        &["deploy", "release"],
        &cwd,
        &[("PATH", stubbed_path(&bin))],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("CARGO_REGISTRY_TOKEN"));

    // cargo missing -> metadata spawn error
    let empty_bin = t.join("empty-bin");
    std::fs::create_dir_all(&empty_bin).unwrap();
    let o = bupha_in(
        &["deploy", "release", "--dry-run"],
        &cwd,
        &[("PATH", empty_bin.display().to_string())],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("cargo metadata"));

    // metadata garbage -> json error
    let bin2 = t.join("bin2");
    std::fs::create_dir_all(&bin2).unwrap();
    stub(&bin2, "cargo", "echo garbage\nexit 0");
    let o = bupha_in(
        &["deploy", "release", "--dry-run"],
        &cwd,
        &[("PATH", stubbed_path(&bin2))],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("metadata json"));

    // mixed versions -> exit 1
    let bin3 = t.join("bin3");
    std::fs::create_dir_all(&bin3).unwrap();
    let fixture3 = t.join("meta3.json");
    std::fs::write(
        &fixture3,
        r#"{"workspace_members":["a","b"],"packages":[
        {"name":"a","id":"a","version":"0.1.0","dependencies":[]},
        {"name":"b","id":"b","version":"0.2.0","dependencies":[]}
    ]}"#,
    )
    .unwrap();
    stub(
        &bin3,
        "cargo",
        &format!("cat \"{}\"\nexit 0", fixture3.display()),
    );
    let o = bupha_in(
        &["deploy", "release", "--dry-run"],
        &cwd,
        &[("PATH", stubbed_path(&bin3))],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("mixed versions"));

    // dep cycle -> publish_order error
    let bin4 = t.join("bin4");
    std::fs::create_dir_all(&bin4).unwrap();
    let fixture4 = t.join("meta4.json");
    std::fs::write(
        &fixture4,
        r#"{"workspace_members":["a","b"],"packages":[
        {"name":"a","id":"a","version":"0.1.0","dependencies":[{"name":"b","path":"x"}]},
        {"name":"b","id":"b","version":"0.1.0","dependencies":[{"name":"a","path":"x"}]}
    ]}"#,
    )
    .unwrap();
    stub(
        &bin4,
        "cargo",
        &format!("cat \"{}\"\nexit 0", fixture4.display()),
    );
    let o = bupha_in(
        &["deploy", "release", "--dry-run"],
        &cwd,
        &[("PATH", stubbed_path(&bin4))],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("dependency cycle"));

    // publish fails permanently -> exit 1 (slow: 4x20s retries)
    let bin5 = t.join("bin5");
    std::fs::create_dir_all(&bin5).unwrap();
    stub(&bin5, "cargo", &format!(
        "case \"$*\" in\n *metadata*) cat \"{}\";;\n *publish*) echo \"error: failed\"; exit 1;;\nesac\nexit 0",
        fixture.display()
    ));
    let o = bupha_in(
        &["deploy", "release"],
        &cwd,
        &[
            ("PATH", stubbed_path(&bin5)),
            ("CARGO_REGISTRY_TOKEN", "t".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("publish failed"));

    // publish ok but git tag fails
    let bin6 = t.join("bin6");
    std::fs::create_dir_all(&bin6).unwrap();
    stub(
        &bin6,
        "cargo",
        &format!(
        "case \"$*\" in\n *metadata*) cat \"{}\";;\n *publish*) echo \"Uploaded\";;\nesac\nexit 0",
        fixture.display()
    ),
    );
    stub(
        &bin6,
        "git",
        "case \"$*\" in\n *tag*|*push*) exit 1;;\n *) exit 0;;\nesac",
    );
    let o = bupha_in(
        &["deploy", "release"],
        &cwd,
        &[
            ("PATH", stubbed_path(&bin6)),
            ("CARGO_REGISTRY_TOKEN", "t".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("git tag"));

    // publish ok, tag exists->skip create? cover push_tag fail arm:
    // rev-parse verify succeeds (tag_exists true) then push fails.
    let bin7 = t.join("bin7");
    std::fs::create_dir_all(&bin7).unwrap();
    stub(
        &bin7,
        "cargo",
        &format!(
        "case \"$*\" in\n *metadata*) cat \"{}\";;\n *publish*) echo \"Uploaded\";;\nesac\nexit 0",
        fixture.display()
    ),
    );
    stub(
        &bin7,
        "git",
        "case \"$*\" in\n *push*) exit 1;;\n *) exit 0;;\nesac",
    );
    let o = bupha_in(
        &["deploy", "release"],
        &cwd,
        &[
            ("PATH", stubbed_path(&bin7)),
            ("CARGO_REGISTRY_TOKEN", "t".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("tag push failed"));
}

#[test]
fn destroy_variants() {
    let t = tmp("dst");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(&bin, "systemctl", "exit 0");

    // destroy service: unit file exists -> removed + printed
    let home = t.join("h1");
    let unit_dir = home.join(".config/systemd/user");
    std::fs::create_dir_all(&unit_dir).unwrap();
    std::fs::write(unit_dir.join("bupha-watch.service"), "x").unwrap();
    let cfgp = cfg(&t, "");
    let o = bupha(
        &["destroy", "service", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", home.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("removed"));
    assert!(!unit_dir.join("bupha-watch.service").exists());

    // --worker removes the worker unit
    let hw = t.join("hw");
    let wdir = hw.join(".config/systemd/user");
    std::fs::create_dir_all(&wdir).unwrap();
    std::fs::write(wdir.join("bupha-worker.service"), "x").unwrap();
    let o = bupha(
        &["destroy", "service", "--worker", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", hw.display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(!wdir.join("bupha-worker.service").exists());

    // daemon-reload failure -> exit 1
    stub(&bin, "systemctl", "exit 1");
    let o = bupha(
        &["destroy", "service", "--config", cfgp.to_str().unwrap()],
        &[
            ("HOME", t.join("h2").display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert_eq!(o.status.code(), Some(1));

    // destroy webhook: no public_url -> exit 1
    let o = bupha(
        &["destroy", "webhook", "--config", cfgp.to_str().unwrap()],
        &[("HOME", t.join("h3").display().to_string())],
    );
    assert_eq!(o.status.code(), Some(1));

    // destroy webhook against fake api (match -> delete; no match; err)
    let api = serve(|req, _| {
        if req.starts_with("GET") {
            (
                200,
                r#"[{"id":9,"config":{"url":"http://ci.example/hook"}}]"#.into(),
            )
        } else {
            (204, "".into())
        }
    });
    let p = t.join("dw.toml");
    std::fs::write(&p, format!(
        "[trigger]\nkind = \"github\"\napi_base = \"{api}\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["destroy", "webhook", "--config", p.to_str().unwrap()],
        &[
            ("HOME", t.join("h4").display().to_string()),
            ("BUPHA_TOKEN", "t".into()),
        ],
    );
    assert!(out_ok(&o).contains("deleted hook 9"), "{}", out_ok(&o));

    let api2 = serve(|_, _| (401, "{}".into()));
    let p2 = t.join("dw2.toml");
    std::fs::write(&p2, format!(
        "[trigger]\nkind = \"github\"\napi_base = \"{api2}\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[tunnel]\nprovider = \"direct\"\npublic_url = \"http://ci.example\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["destroy", "webhook", "--config", p2.to_str().unwrap()],
        &[
            ("HOME", t.join("h5").display().to_string()),
            ("BUPHA_TOKEN", "t".into()),
        ],
    );
    assert_eq!(o.status.code(), Some(1));

    // destroy worktrees: stray worktree dirs + queue.json removed
    let base = t.join("wtbase");
    std::fs::create_dir_all(&base).unwrap();
    let orphan = t.join("wtbase-o-r-deadbeefdead");
    std::fs::create_dir_all(&orphan).unwrap();
    std::fs::write(orphan.join(".git"), "gitdir: /x").unwrap(); // file, not dir
    let notorphan = t.join("wtbase-o-r-other");
    std::fs::create_dir_all(notorphan.join(".git")).unwrap(); // .git dir -> kept
    std::fs::create_dir_all(t.join("unrelated")).unwrap();
    std::fs::write(base.join("queue.json"), "{}").unwrap();
    let p3 = t.join("dwt.toml");
    std::fs::write(&p3, format!(
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[worktree]\nbase = \"{}\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
        base.display(), repo_dir.display()
    )).unwrap();
    let o = bupha(
        &["destroy", "worktrees", "--config", p3.to_str().unwrap()],
        &[
            ("HOME", t.join("h6").display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(!orphan.exists() && notorphan.exists());
    assert!(!base.join("queue.json").exists());

    // base="/" -> parent() is None (no read_dir at all); queue.json
    // absent; watched() errors with no [[repo]] -> all three
    // skip-paths in one shot.
    let p4 = t.join("dwt2.toml");
    std::fs::write(
        &p4,
        "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[worktree]\nbase = \"/\"\n",
    )
    .unwrap();
    let o = bupha(
        &["destroy", "worktrees", "--config", p4.to_str().unwrap()],
        &[("HOME", t.join("h7").display().to_string())],
    );
    assert!(o.status.success(), "{}", out_ok(&o));

    // base under a regular FILE -> parent() resolves but read_dir
    // fails (ENOTDIR) -> the read_dir Err arm.
    let blocker = t.join("afile");
    std::fs::write(&blocker, "x").unwrap();
    let p5 = t.join("dwt3.toml");
    std::fs::write(
        &p5,
        format!(
            "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[worktree]\nbase = \"{}\"\n[[repo]]\nname = \"o/r\"\ndir = \"{}\"\n",
            blocker.join("sub").display(),
            repo_dir.display()
        ),
    )
    .unwrap();
    let o = bupha(
        &["destroy", "worktrees", "--config", p5.to_str().unwrap()],
        &[
            ("HOME", t.join("h8").display().to_string()),
            ("PATH", stubbed_path(&bin)),
        ],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
}

#[test]
fn runs_log_explain() {
    let t = tmp("rle");
    let repo_dir = t.join("repo");
    let sha = repo(&repo_dir);
    let base = t.join("wt");
    std::fs::create_dir_all(&base).unwrap();
    let cfgp = cfg_repo(&t, &repo_dir, "");
    let env: Vec<(&str, String)> = vec![
        ("HOME", t.join("h").display().to_string()),
        ("XDG_CONFIG_HOME", t.join("xdg").display().to_string()),
    ];

    // no history yet
    let o = bupha(&["runs", "--config", cfgp.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("no runs yet"));
    let o = bupha(&["log", "--config", cfgp.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("no log yet"));

    // history: good line + garbage + second repo line
    std::fs::write(
        base.join("history.jsonl"),
        format!(
            "{{\"sha\":\"{sha}\",\"repo\":\"o/r\",\"status\":\"success\",\"wall_secs\":9}}\nGARBAGE\n{{\"sha\":\"{}\",\"repo\":\"o/other\",\"status\":\"failure\",\"wall_secs\":2}}\n",
            "f".repeat(40)
        ),
    )
    .unwrap();
    let o = bupha(&["runs", "--config", cfgp.to_str().unwrap()], &env);
    let s = out_ok(&o);
    assert!(
        s.contains("o/r") && s.contains("success") && s.contains("9s"),
        "{s}"
    );
    let o = bupha(
        &["runs", "o/other", "--config", cfgp.to_str().unwrap()],
        &env,
    );
    let s = out_ok(&o);
    assert!(s.contains("o/other") && !s.contains("o/r "), "{s}");
    let o = bupha(
        &["log", "-n", "1", "--config", cfgp.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("o/other"));

    // explain via history
    let o = bupha(
        &["explain", &sha[..8], "--config", cfgp.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("ran: success"), "{}", out_ok(&o));
    let o = bupha(
        &["explain", "ffffffff", "--config", cfgp.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("ran: failure"));

    // explain via queue store: running + pending
    let qsha = "c".repeat(40);
    let store = RedbQueueStore::open(base.join("queue.redb")).unwrap();
    let mut m = BTreeMap::new();
    m.insert(
        "o/r".to_string(),
        PersistedQueue {
            pending: VecDeque::from(["d".repeat(40)]),
            running: Some(qsha.clone()),
        },
    );
    store.persist(&m).unwrap();
    drop(store);
    let o = bupha(
        &["explain", &qsha[..8], "--config", cfgp.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("running now"), "{}", out_ok(&o));
    let o = bupha(
        &[
            "explain",
            &"d".repeat(8),
            "--config",
            cfgp.to_str().unwrap(),
        ],
        &env,
    );
    assert!(out_ok(&o).contains("queued on o/r"), "{}", out_ok(&o));

    // explain via leftover worktree dir
    let wtsha = "e".repeat(12);
    std::fs::create_dir_all(base.join(format!("x-{wtsha}"))).unwrap();
    let o = bupha(
        &["explain", &wtsha, "--config", cfgp.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("worktree exists"), "{}", out_ok(&o));

    // nowhere -> never seen
    let o = bupha(
        &["explain", "12345678", "--config", cfgp.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("never seen"));

    // missing worktree.base entirely -> every explain source takes
    // its Err arm: no history file, queue store won't open, worktree
    // read_dir fails.
    let p_gone = t.join("gone.toml");
    std::fs::write(
        &p_gone,
        format!(
            "[trigger]\nkind = \"github\"\n[runner]\nkind = \"shell\"\n[status]\nkind = \"log\"\n[worktree]\nbase = \"{}\"\n",
            t.join("missing-base").display()
        ),
    )
    .unwrap();
    let o = bupha(
        &["explain", "deadbeef", "--config", p_gone.to_str().unwrap()],
        &env,
    );
    assert!(out_ok(&o).contains("never seen"), "{}", out_ok(&o));
}

#[test]
fn runs_filter_matches_job_keys() {
    // Job runs record repo as "name:job"; a bare repo filter must
    // catch them, a job filter stays exact.
    let t = tmp("rjob");
    let repo_dir = t.join("repo");
    let sha = repo(&repo_dir);
    let base = t.join("wt");
    std::fs::create_dir_all(&base).unwrap();
    let cfgp = cfg_repo(&t, &repo_dir, "");
    let env: Vec<(&str, String)> = vec![("HOME", t.join("h").display().to_string())];
    std::fs::write(
        base.join("history.jsonl"),
        format!(
            "{{\"sha\":\"{sha}\",\"repo\":\"o/r:gate\",\"status\":\"success\",\"wall_secs\":1}}\n{{\"sha\":\"{}\",\"repo\":\"o/r:docs\",\"status\":\"failure\",\"wall_secs\":2}}\n{{\"sha\":\"{}\",\"repo\":\"o/other\",\"status\":\"success\",\"wall_secs\":3}}\n",
            "a".repeat(40),
            "b".repeat(40)
        ),
    )
    .unwrap();
    let o = bupha(&["runs", "o/r", "--config", cfgp.to_str().unwrap()], &env);
    let s = out_ok(&o);
    assert!(
        s.contains("o/r:gate") && s.contains("o/r:docs") && !s.contains("o/other"),
        "repo filter catches job keys: {s}"
    );
    let o = bupha(
        &["runs", "o/r:gate", "--config", cfgp.to_str().unwrap()],
        &env,
    );
    let s = out_ok(&o);
    assert!(
        s.contains("o/r:gate") && !s.contains("o/r:docs"),
        "job filter stays exact: {s}"
    );
}

#[test]
fn init_variants() {
    let t = tmp("init");
    let bin = t.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    stub(
        &bin,
        "dagger",
        &format!("echo \"$@\" >> \"{}/dcalls\"\nexit 0", t.display()),
    );
    let env = vec![("PATH", stubbed_path(&bin))];

    // fresh dir: ci.sh + dagger init + install + delegate template
    let d = t.join("my repo"); // space -> name sanitized
    std::fs::create_dir_all(&d).unwrap();
    let o = bupha(&["init", d.to_str().unwrap()], &env);
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(d.join("ci.sh").exists());
    assert!(d.join("dagger/src/ci/main.py").exists());
    let main_py = std::fs::read_to_string(d.join("dagger/src/ci/main.py")).unwrap();
    assert!(main_py.contains("repo=\"my-repo\""));
    let calls = std::fs::read_to_string(t.join("dcalls")).unwrap();
    assert!(calls.contains("init --sdk=python --name=ci"));
    assert!(calls.contains("install github.com/9aia/bupha/modules/dagger@bupha-v"));

    // git remote drives PATCH_HOST (git@ -> https://)
    let d2 = t.join("r2");
    std::fs::create_dir_all(&d2).unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&d2)
        .output()
        .unwrap();
    Command::new("git")
        .args(["remote", "add", "origin", "git@github.com:o/r.git"])
        .current_dir(&d2)
        .output()
        .unwrap();
    let o = bupha(&["init", d2.to_str().unwrap()], &env);
    assert!(o.status.success());
    let main_py = std::fs::read_to_string(d2.join("dagger/src/ci/main.py")).unwrap();
    assert!(
        main_py.contains("patch_host=\"https://github.com/o/r\""),
        "{main_py}"
    );

    // https remote with .git suffix -> stripped
    let d3 = t.join("r3");
    std::fs::create_dir_all(&d3).unwrap();
    Command::new("git")
        .args(["init", "-q"])
        .current_dir(&d3)
        .output()
        .unwrap();
    Command::new("git")
        .args(["remote", "add", "origin", "https://x/y.git"])
        .current_dir(&d3)
        .output()
        .unwrap();
    let o = bupha(&["init", d3.to_str().unwrap()], &env);
    assert!(o.status.success());
    assert!(std::fs::read_to_string(d3.join("dagger/src/ci/main.py"))
        .unwrap()
        .contains("patch_host=\"https://x/y\""));

    // existing files are left alone; install failure is only a warning
    let d4 = t.join("r4");
    std::fs::create_dir_all(&d4).unwrap();
    std::fs::write(d4.join("ci.sh"), "custom").unwrap();
    std::fs::write(d4.join("dagger.json"), "{}").unwrap();
    std::fs::create_dir_all(d4.join("dagger/src/ci")).unwrap();
    std::fs::write(d4.join("dagger/src/ci/main.py"), "custom-py").unwrap();
    let bin2 = t.join("bin2");
    std::fs::create_dir_all(&bin2).unwrap();
    stub(
        &bin2,
        "dagger",
        "case \"$1\" in\n install) exit 1;;\n *) exit 0;;\nesac",
    );
    let o = bupha(
        &["init", d4.to_str().unwrap()],
        &[("PATH", stubbed_path(&bin2))],
    );
    assert!(o.status.success(), "{}", out_ok(&o));
    assert!(out_ok(&o).contains("dagger install failed"));
    assert_eq!(std::fs::read_to_string(d4.join("ci.sh")).unwrap(), "custom");
    assert_eq!(
        std::fs::read_to_string(d4.join("dagger/src/ci/main.py")).unwrap(),
        "custom-py"
    );

    // dagger init failure -> exit 1
    let bin3 = t.join("bin3");
    std::fs::create_dir_all(&bin3).unwrap();
    stub(&bin3, "dagger", "exit 1");
    let d5 = t.join("r5");
    std::fs::create_dir_all(&d5).unwrap();
    let o = bupha(
        &["init", d5.to_str().unwrap()],
        &[("PATH", stubbed_path(&bin3))],
    );
    assert_eq!(o.status.code(), Some(1));
    assert!(out_ok(&o).contains("dagger init failed"));

    // ci.sh write failure -> warning only (dangling symlink target)
    let d6 = t.join("r6");
    std::fs::create_dir_all(&d6).unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink("/nonexistent-dir-xyz/x", d6.join("ci.sh")).unwrap();
    let o = bupha(&["init", d6.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("ci.sh:"), "{}", out_ok(&o));

    // dagger template write failure -> warning only (dagger path is a file)
    let d7 = t.join("r7");
    std::fs::create_dir_all(&d7).unwrap();
    std::fs::write(d7.join("dagger"), "not-a-dir").unwrap();
    let o = bupha(&["init", d7.to_str().unwrap()], &env);
    assert!(out_ok(&o).contains("template:"), "{}", out_ok(&o));
}

#[test]
fn worker_requires_shared_queue() {
    let t = tmp("wkq");
    let repo_dir = t.join("repo");
    repo(&repo_dir);
    let cfgp = cfg_repo(&t, &repo_dir, "");
    // kind=redb (default) -> exit 2: no pull queue to claim from.
    let o = bupha(
        &["worker", "--config", cfgp.to_str().unwrap()],
        &[("HOME", t.join("h").display().to_string())],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("redis"), "{}", out_ok(&o));
    // kind=redis without url -> exit 2 as well.
    let cfgp2 = cfg_repo(&t, &repo_dir, "[queue]\nkind = \"redis\"\n");
    let o = bupha(
        &["worker", "--config", cfgp2.to_str().unwrap()],
        &[("HOME", t.join("h2").display().to_string())],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("url"), "{}", out_ok(&o));
    // watched() rejects the config (no [[repo]] entries) -> "config:" + exit 2.
    let cfgp3 = cfg(&t, "[queue]\nkind = \"redis\"\nurl = \"redis://127.0.0.1:1/\"\n");
    let o = bupha(
        &["worker", "--config", cfgp3.to_str().unwrap()],
        &[("HOME", t.join("h3").display().to_string())],
    );
    assert_eq!(o.status.code(), Some(2));
    assert!(out_ok(&o).contains("config:"), "{}", out_ok(&o));
}
