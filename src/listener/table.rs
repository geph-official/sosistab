use std::{collections::BTreeMap, net::SocketAddr, sync::Arc, time::Instant};

use crate::{buffer::Buff, SVec, SessionBack};

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
            .filter(|(_, (_, usage))| usage.elapsed().as_millis() < 10000)
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
            tracing::trace!("sending down most recent {}", most_recent);
            most_recent
        } else {
            let random =
                recently_used_shards[rand::thread_rng().gen_range(0, recently_used_shards.len())];
            tracing::trace!("sending down random {}", random);
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

#[derive(Default, Clone)]
pub(crate) struct SessionTable {
    token_to_sess: Arc<RwLock<BTreeMap<Buff, SessEntry>>>,
    addr_to_token: Arc<RwLock<BTreeMap<SocketAddr, Buff>>>,
}

impl SessionTable {
    pub fn rebind(&self, addr: SocketAddr, shard_id: u8, token: Buff) -> bool {
        let token_to_sess = self.token_to_sess.write();
        let mut addr_to_token = self.addr_to_token.write();
        if let Some(entry) = token_to_sess.get(&token) {
            let old = entry.addrs.write().insert_addr(shard_id, addr);
            tracing::trace!("binding {}=>{}", shard_id, addr);
            if let Some(old) = old {
                addr_to_token.remove(&old);
            }
            addr_to_token.insert(addr, token);
            true
        } else {
            false
        }
    }
    pub fn delete(&self, token: Buff) {
        let mut token_to_sess = self.token_to_sess.write();
        let mut addr_to_token = self.addr_to_token.write();
        if let Some(entry) = token_to_sess.remove(&token) {
            for (addr, _) in entry.addrs.read().map.values() {
                addr_to_token.remove(addr);
            }
        }
    }

    pub fn lookup(&self, addr: SocketAddr) -> Option<Arc<SessionBack>> {
        let token_to_sess = self.token_to_sess.read();
        let addr_to_token = self.addr_to_token.read();
        let token = addr_to_token.get(&addr)?;
        let entry = token_to_sess.get(token)?;
        Some(entry.session_back.clone())
    }

    pub fn new_sess(
        &self,
        token: Buff,
        session_back: Arc<SessionBack>,
        locked_addrs: Arc<RwLock<ShardedAddrs>>,
    ) {
        let mut token_to_sess = self.token_to_sess.write();
        let entry = SessEntry {
            session_back,
            addrs: locked_addrs,
        };
        token_to_sess.insert(token, entry);
    }
}
