//! RedisQueueStore + TryClaim against a fake redis speaking just
//! enough RESP (arrays of bulk strings in, simple/bulk/int/err out).
//! Covers persist/load, claim fencing (SET NX PX + INCR), and the
//! Lua renew (EVALSHA → 1/0/error).

use bupha_queue::{PersistedQueue, QueueStore};
use bupha_queue_lease::TryClaim;
use bupha_queue_redis::RedisQueueStore;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

type Map = Arc<Mutex<HashMap<String, String>>>;

fn read_cmd(r: &mut BufReader<std::net::TcpStream>) -> Option<Vec<String>> {
    let mut line = String::new();
    r.read_line(&mut line).ok()?;
    let line = line.trim();
    if line.is_empty() {
        return None;
    }
    let n: usize = line.strip_prefix('*')?.parse().ok()?;
    let mut argv = Vec::with_capacity(n);
    for _ in 0..n {
        let mut hdr = String::new();
        r.read_line(&mut hdr).ok()?;
        let len: usize = hdr.trim().strip_prefix('$')?.parse().ok()?;
        let mut buf = vec![0u8; len + 2];
        r.read_exact(&mut buf).ok()?;
        argv.push(String::from_utf8_lossy(&buf[..len]).to_string());
    }
    Some(argv)
}

/// One fake redis server; renew replies `-ERR` when the worker arg is
/// "boom" (exercises the Err arm of renew_inner).
fn serve() -> String {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let map: Map = Arc::new(Mutex::new(HashMap::new()));
    std::thread::spawn(move || {
        for stream in l.incoming() {
            let Ok(s) = stream else { continue };
            let map = map.clone();
            std::thread::spawn(move || {
                let mut w = s.try_clone().unwrap();
                let mut r = BufReader::new(s);
                while let Some(argv) = read_cmd(&mut r) {
                    let cmd = argv[0].to_uppercase();
                    let resp = match cmd.as_str() {
                        "SET" => {
                            let (k, v) = (argv[1].clone(), argv[2].clone());
                            let nx = argv.iter().any(|a| a.eq_ignore_ascii_case("nx"));
                            let mut m = map.lock().unwrap();
                            if nx && m.contains_key(&k) {
                                "$-1\r\n".to_string()
                            } else {
                                m.insert(k, v);
                                "+OK\r\n".to_string()
                            }
                        }
                        "GET" => match map.lock().unwrap().get(&argv[1]) {
                            Some(v) => format!("${}\r\n{v}\r\n", v.len()),
                            None => "$-1\r\n".to_string(),
                        },
                        // redis-rs sends INCRBY (delta argv[2]) for
                        // the fencing counter, never bare INCR.
                        "INCR" | "INCRBY" => {
                            let d: u64 = argv.get(2).and_then(|s| s.parse().ok()).unwrap_or(1);
                            let mut m = map.lock().unwrap();
                            let e = m.entry(argv[1].clone()).or_insert_with(|| "0".into());
                            let n: u64 = e.parse().unwrap_or(0) + d;
                            *e = n.to_string();
                            format!(":{n}\r\n")
                        }
                        // EVALSHA/EVAL: keys[1] is argv[3], argv[4]=worker,
                        // argv[5]=ttl — mirror the real Lua script.
                        "EVALSHA" | "EVAL" => {
                            let (key, worker) = (argv[3].clone(), argv[4].clone());
                            if worker == "boom" {
                                "-ERR boom\r\n".to_string()
                            } else {
                                let held = map.lock().unwrap().get(&key).cloned();
                                if held.as_deref() == Some(worker.as_str()) {
                                    ":1\r\n".to_string()
                                } else {
                                    ":0\r\n".to_string()
                                }
                            }
                        }
                        _ => "+OK\r\n".to_string(),
                    };
                    if w.write_all(resp.as_bytes()).is_err() {
                        break;
                    }
                }
            });
        }
    });
    format!("redis://127.0.0.1:{port}/")
}

#[test]
fn persist_load_roundtrip() {
    let s = RedisQueueStore::open(&serve()).unwrap();
    let mut m = BTreeMap::new();
    m.insert(
        "org/api".to_string(),
        PersistedQueue {
            pending: VecDeque::from(["aaa".to_string(), "bbb".to_string()]),
            running: Some("ccc".to_string()),
        },
    );
    s.persist(&m).unwrap();
    let back = s.load().unwrap();
    assert_eq!(back["org/api"].pending.len(), 2);
    assert_eq!(back["org/api"].running.as_deref(), Some("ccc"));
}

#[test]
fn load_empty_and_bad_json() {
    let url = serve();
    let s = RedisQueueStore::open(&url).unwrap();
    assert!(s.load().unwrap().is_empty());

    // Poison the stored blob -> InvalidData error, not a panic.
    {
        let m = std::sync::Mutex::new(());
        let _g = m.lock().unwrap();
    }
    let s2 = RedisQueueStore::open(&url).unwrap();
    // Persist then corrupt via a second store? Instead write garbage
    // directly through the fake's SET by issuing persist of normal
    // data, then overwrite — simplest: another persist of garbage
    // isn't possible via the API, so reach in through a raw socket.
    use std::net::TcpStream;
    let port = url
        .trim_start_matches("redis://")
        .trim_end_matches('/')
        .to_string();
    let mut raw = TcpStream::connect(port).unwrap();
    let blob = "not json";
    write!(
        raw,
        "*3\r\n$3\r\nSET\r\n$11\r\nbupha:queue\r\n${}\r\n{blob}\r\n",
        blob.len()
    )
    .unwrap();
    let mut r = BufReader::new(raw);
    let mut line = String::new();
    r.read_line(&mut line).unwrap();
    assert!(line.starts_with("+OK"));
    let e = s2.load().unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::InvalidData);
}

#[test]
fn claim_fences_and_excludes() {
    let s = RedisQueueStore::open(&serve()).unwrap();
    let f1 = s.try_claim("org/api", "sha1", "worker-a").unwrap();
    assert!(f1.is_some());
    // Second worker is excluded while the lease holds.
    assert!(s
        .try_claim("org/api", "sha1", "worker-b")
        .unwrap()
        .is_none());
    // Fences are monotonic across different shas.
    let f3 = s.try_claim("org/api", "sha2", "worker-b").unwrap().unwrap();
    assert!(f3.0 > f1.unwrap().0);
}

#[test]
fn renew_paths() {
    let s = RedisQueueStore::open(&serve()).unwrap();
    s.try_claim("org/api", "r-sha", "worker-a").unwrap();
    assert!(s.renew("org/api", "r-sha", "worker-a").unwrap());
    assert!(!s.renew("org/api", "r-sha", "stranger").unwrap());
    // Server-side error propagates.
    assert!(s.renew("org/api", "r-sha", "boom").is_err());
}

#[test]
fn open_errors() {
    assert!(RedisQueueStore::open("not-a-url").is_err());
    // Bound then released port -> connection refused.
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    drop(l);
    assert!(RedisQueueStore::open(&format!("redis://127.0.0.1:{port}/")).is_err());
}
