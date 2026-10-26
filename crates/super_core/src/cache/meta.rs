//! What's known of a cached response: its header, and until when it can be served

use std::{
    sync::Arc,
    time::{Duration, SystemTime},
};

use http::HeaderMap;

use crate::http::ResponseHeader;

#[derive(Debug)]
struct Inner {
    fresh_until: SystemTime,
    created: SystemTime,
    stale_while_revalidate_sec: u32,
    stale_if_error_sec: u32,
    header: ResponseHeader,
}

/// A cached response's header and freshness (cheap to clone)
#[derive(Debug, Clone)]
pub struct CacheMeta(Arc<Inner>);

impl CacheMeta {
    pub fn new(
        fresh_until: SystemTime,
        created: SystemTime,
        stale_while_revalidate_sec: u32,
        stale_if_error_sec: u32,
        header: ResponseHeader,
    ) -> Self {
        Self(Arc::new(Inner {
            fresh_until,
            created,
            stale_while_revalidate_sec,
            stale_if_error_sec,
            header,
        }))
    }

    pub fn created(&self) -> SystemTime {
        self.0.created
    }

    pub fn fresh_until(&self) -> SystemTime {
        self.0.fresh_until
    }

    pub fn is_fresh(&self, time: SystemTime) -> bool {
        time < self.0.fresh_until
    }

    /// How long ago it was cached
    pub fn age(&self) -> Duration {
        SystemTime::now()
            .duration_since(self.0.created)
            .unwrap_or_default()
    }

    pub fn stale_while_revalidate_sec(&self) -> u32 {
        self.0.stale_while_revalidate_sec
    }

    pub fn stale_if_error_sec(&self) -> u32 {
        self.0.stale_if_error_sec
    }

    /// Whether, stale at `time`, it can still be served while it's fetched again
    pub fn serve_stale_while_revalidate(&self, time: SystemTime) -> bool {
        self.stale_within(time, self.0.stale_while_revalidate_sec)
    }

    /// Whether, stale at `time`, it can still be served when the upstream fails
    pub fn serve_stale_if_error(&self, time: SystemTime) -> bool {
        self.stale_within(time, self.0.stale_if_error_sec)
    }

    fn stale_within(&self, time: SystemTime, seconds: u32) -> bool {
        let until = self.0.fresh_until + Duration::from_secs(u64::from(seconds));
        time < until
    }

    pub fn response_header(&self) -> &ResponseHeader {
        &self.0.header
    }

    pub fn headers(&self) -> &HeaderMap {
        &self.0.header.headers
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, SystemTime};

    use super::CacheMeta;
    use crate::http::ResponseHeader;

    #[test]
    fn test_fresh_then_stale() {
        let now = SystemTime::now();
        let header = ResponseHeader::build(200, None).unwrap();
        let meta = CacheMeta::new(now + Duration::from_secs(10), now, 5, 20, header);

        assert!(meta.is_fresh(now));
        let later = now + Duration::from_secs(12);
        assert!(!meta.is_fresh(later));
        assert!(meta.serve_stale_while_revalidate(later));
        assert!(meta.serve_stale_if_error(later));
        let much_later = now + Duration::from_secs(25);
        assert!(!meta.serve_stale_while_revalidate(much_later));
        assert!(meta.serve_stale_if_error(much_later));
    }
}
