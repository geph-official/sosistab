use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use dashmap::DashMap;
use once_cell::sync::Lazy;
use parking_lot::Mutex;

// recently seen tracker
pub(crate) struct RecentFilter {
    seen: HashMap<blake3::Hash, Instant>,
    expiry: VecDeque<(Instant, blake3::Hash)>,
}

impl RecentFilter {
    fn new() -> Self {
        RecentFilter {
            seen: Default::default(),
            expiry: Default::default(),
        }
    }

    pub fn check(&mut self, val: &[u8]) -> bool {
        // clean up first
        while let Some(to_delete) = self.expiry.front().and_then(|(expiry, hash)| {
            if expiry.elapsed() > Duration::from_secs(600) {
                Some(*hash)
            } else {
                None
            }
        }) {
            let _ = self.expiry.pop_front();
            self.seen.remove(&to_delete);
        }
        // then add
        let key = blake3::hash(val);
        if let Some(time) = self.seen.get(&key) {
            tracing::error!("replay from {:?} ago", time.elapsed());
            false
        } else {
            let now = Instant::now();
            self.seen.insert(key, now);
            self.expiry.push_back((now, key));
            true
        }
    }
}

/// A global recent filter.
pub(crate) static RECENT_FILTER: Lazy<Mutex<RecentFilter>> =
    Lazy::new(|| Mutex::new(RecentFilter::new()));
