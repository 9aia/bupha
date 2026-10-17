//! sccache impl of `Cache` — renders the sccache conf string.
//! sccache itself speaks file/S3/R2/GCS backends; this impl is the
//! single source for how that conf is produced for runners.

use bupha_cache::Cache;

/// A sccache backend described by its conf value (path or URL —
/// "file:///srv/cache", "s3://bucket/prefix", ...).
pub struct SccacheConf {
    pub conf: Option<String>,
}

impl Cache for SccacheConf {
    fn kind(&self) -> &'static str { "sccache" }
    fn conf(&self) -> Option<String> { self.conf.clone() }
}
