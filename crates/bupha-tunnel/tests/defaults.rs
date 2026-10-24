//! Port default: providers whose URL only exists at runtime return
//! None from `public_url`.

use bupha_tunnel::Tunnel;
use std::io;
use std::process::Child;

struct Direct;

impl Tunnel for Direct {
    fn name(&self) -> &'static str {
        "direct"
    }
    fn spawn(&self) -> io::Result<Option<Child>> {
        Ok(None)
    }
}

#[test]
fn default_public_url_is_none() {
    let t = Direct;
    assert_eq!(t.name(), "direct");
    assert!(t.public_url().is_none());
    assert!(t.spawn().unwrap().is_none());
}
