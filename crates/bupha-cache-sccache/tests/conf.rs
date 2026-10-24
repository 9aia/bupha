//! SccacheConf — kind id and conf passthrough.

use bupha_cache::Cache;
use bupha_cache_sccache::SccacheConf;

#[test]
fn kind_and_conf_passthrough() {
    let c = SccacheConf {
        conf: Some("file:///srv/cache".into()),
    };
    assert_eq!(c.kind(), "sccache");
    assert_eq!(c.conf().as_deref(), Some("file:///srv/cache"));

    let off = SccacheConf { conf: None };
    assert_eq!(off.kind(), "sccache");
    assert!(off.conf().is_none());
}
