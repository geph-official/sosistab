use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Instant};

use crate::{SVec, SessionBack};
use bytes::Bytes;
use parking_lot::RwLock;
use rand::Rng;
use rustc_hash::FxHashMap;

pub struct ShardedAddrs {
    // maps shard ID to socketaddr and last update time
    map: FxHashMap<u8, (SocketAddr, Instant)>,
}

impl ShardedAddrs {
    /// Creates a new table of shard addresses.
    pub fn new(initial_shard: u8, initial_addr: SocketAddr) -> Self {
        let mut map = FxHashMap::default();
        map.insert(initial_shard, (initial_addr, Instant::now()));
        Self { map }
    }

    /// Gets the most appropriate address to send a packet down.
    pub fn get_addr(&self) -> SocketAddr {
        // svec to prevent allocating in such an extremely hot path
        let recently_used_shards = self
            .map
            .iter()
            .filter(|(_, (_, usage))| usage.elapsed().as_millis() < 1000)
            .map(|f| f.1 .0)
            .collect::<SVec<_>>();
        // if no recently used, then push the most recently used one
        if recently_used_shards.is_empty() {
            let (most_recent, _) = self
                .map
                .values()
                .max_by_key(|v| v.1)
                .copied()
                .expect("no shards at all");
            tracing::debug!("sending down most recent {}", most_recent);
            most_recent
        } else {
            let random =
                recently_used_shards[rand::thread_rng().gen_range(0, recently_used_shards.len())];
            tracing::debug!("sending down random {}", random);
            random
        }
    }

    /// Sets an index to a particular address
    pub fn insert_addr(&mut self, index: u8, addr: SocketAddr) -> Option<SocketAddr> {
        self.map.insert(index, (addr, Instant::now())).map(|v| v.0)
    }
}

struct SessEntry {
    session_back: Arc<SessionBack>,
    addrs: Arc<RwLock<ShardedAddrs>>,
}

#[derive(Default)]
pub(crate) struct SessionTable {
    token_to_sess: BTreeMap<Bytes, SessEntry>,
    addr_to_token: BTreeMap<SocketAddr, Bytes>,
}

impl SessionTable {
    #[tracing::instrument(skip(self), level = "trace")]
    pub fn rebind(&mut self, addr: SocketAddr, shard_id: u8, token: Bytes) -> bool {
        if let Some(entry) = self.token_to_sess.get(&token) {
            let old = entry.addrs.write().insert_addr(shard_id, addr);
            tracing::trace!("binding {}=>{}", shard_id, addr);
            if let Some(old) = old {
                self.addr_to_token.remove(&old);
            }
            self.addr_to_token.insert(addr, token);
            true
        } else {
            false
        }
    }

    #[tracing::instrument(skip(self), level = "trace")]
    pub fn delete(&mut self, token: Bytes) {
        if let Some(entry) = self.token_to_sess.remove(&token) {
            for (addr, _) in entry.addrs.read().map.values() {
                self.addr_to_token.remove(addr);
            }
        }
    }

    #[tracing::instrument(skip(self), level = "trace")]
    pub fn lookup(&self, addr: SocketAddr) -> Option<&SessionBack> {
        let token = self.addr_to_token.get(&addr)?;
        let entry = self.token_to_sess.get(token)?;
        Some(&entry.session_back)
    }

    #[tracing::instrument(skip(self, session_back, locked_addrs), level = "trace")]
    pub fn new_sess(
        &mut self,
        token: Bytes,
        session_back: Arc<SessionBack>,
        locked_addrs: Arc<RwLock<ShardedAddrs>>,
    ) {
        let entry = SessEntry {
            session_back,
            addrs: locked_addrs,
        };
        self.token_to_sess.insert(token, entry);
    }
}
