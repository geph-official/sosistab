use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use crate::{
    buffer::Buff,
    crypt::{AeadError, NgAead},
    fec::{pre_encode, FrameDecoder},
    protocol::DataFrameV2,
    Role, SVec,
};
use cached::{Cached, SizedCache};
use moka::sync::Cache;
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use rustc_hash::{FxHashMap, FxHashSet};

use super::{rloss::RecvLossCalc, stats::StatsCalculator};

/// I/O-free receiving machine.
pub(crate) struct RecvMachine {
    oob_decoder: OobDecoder,
    rloss: Arc<Mutex<RecvLossCalc>>,
    recv_crypt: NgAead,
    replay_filter: ReplayFilter,
    ping_calc: Arc<StatsCalculator>,
}

static TOTAL_MACHINES: AtomicUsize = AtomicUsize::new(0);

impl Drop for RecvMachine {
    fn drop(&mut self) {
        TOTAL_MACHINES.fetch_sub(1, Ordering::Relaxed);
    }
}

impl RecvMachine {
    /// Creates a new machine based on a version and a down decrypter.
    pub fn new(
        calculator: Arc<StatsCalculator>,
        rloss: Arc<Mutex<RecvLossCalc>>,
        session_key: &[u8],
        direction: Role,
    ) -> Self {
        let count = TOTAL_MACHINES.fetch_add(1, Ordering::Relaxed);
        eprintln!("***** {count} RecvMachines *****");
        let recv_crypt_key = match direction {
            Role::Server => blake3::keyed_hash(crate::crypt::UP_KEY, session_key),
            Role::Client => blake3::keyed_hash(crate::crypt::DN_KEY, session_key),
        };
        let recv_crypt = NgAead::new(recv_crypt_key.as_bytes());

        Self {
            oob_decoder: OobDecoder::new(),
            rloss,
            recv_crypt,
            replay_filter: ReplayFilter::default(),
            ping_calc: calculator,
        }
    }

    /// Processes a single frame. If successfully decoded, return the inner data.
    pub fn process(&mut self, packet: &[u8]) -> Result<Option<SVec<(Buff, u64)>>, AeadError> {
        self.process_ng(packet)
    }

    fn process_ng(&mut self, packet: &[u8]) -> Result<Option<SVec<(Buff, u64)>>, AeadError> {
        let plain_frame = self.recv_crypt.decrypt(packet)?;
        let v2frame = DataFrameV2::depad(&plain_frame);
        match v2frame {
            Some((
                DataFrameV2::Data {
                    frame_no,
                    high_recv_frame_no,
                    total_recv_frames,
                    body,
                },
                loss_rate,
            )) => {
                if !self.replay_filter.add(frame_no) {
                    return Ok(None);
                }
                self.rloss.lock().record(frame_no);
                self.ping_calc.incoming(
                    frame_no,
                    high_recv_frame_no,
                    total_recv_frames,
                    if loss_rate != 0xff {
                        Some((loss_rate as f64) / 255.0)
                    } else {
                        None
                    },
                );
                self.oob_decoder.insert_data(frame_no, body.clone());
                Ok(Some(smallvec::smallvec![(body, frame_no)]))
            }
            Some((
                DataFrameV2::Parity {
                    data_frame_first,
                    data_count,
                    parity_count,
                    parity_index,
                    pad_size,
                    body,
                },
                _,
            )) => {
                let res = self.oob_decoder.insert_parity(
                    ParitySpaceKey {
                        first_data: data_frame_first,
                        data_len: data_count,
                        parity_len: parity_count,
                        pad_size,
                    },
                    parity_index,
                    body,
                );
                let mut toret = SVec::new();
                for (i, body) in res {
                    if self.replay_filter.add(i) {
                        toret.push((body, i));
                    }
                }
                if !toret.is_empty() {
                    tracing::trace!("reconstructed {} packets", toret.len());
                    Ok(Some(toret))
                } else {
                    Ok(None)
                }
            }
            None => Ok(None),
        }
    }
}

/// A filter for replays. Records recently seen seqnos and rejects either repeats or really old seqnos.
#[derive(Debug, Default)]
struct ReplayFilter {
    top_seqno: u64,
    bottom_seqno: u64,
    seen_seqno: FxHashSet<u64>,
}

impl ReplayFilter {
    fn add(&mut self, seqno: u64) -> bool {
        if seqno < self.bottom_seqno {
            // out of range. we can't know, so we just say no
            return false;
        }
        // check the seen
        if self.seen_seqno.contains(&seqno) {
            return false;
        }
        self.seen_seqno.insert(seqno);
        self.top_seqno = seqno.max(self.top_seqno);
        while self.top_seqno - self.bottom_seqno > 10000 {
            self.seen_seqno.remove(&self.bottom_seqno);
            self.bottom_seqno += 1;
        }
        true
    }
}

/// An out-of-band FEC reconstructor
struct OobDecoder {
    data_frames: SizedCache<u64, Buff>,
    parity_space: SizedCache<ParitySpaceKey, FxHashMap<u8, Buff>>,
}

#[derive(Hash, PartialEq, Eq, PartialOrd, Ord, Clone, Copy, Debug)]
struct ParitySpaceKey {
    first_data: u64,
    data_len: u8,
    parity_len: u8,
    pad_size: usize,
}

/// Disable all out-of-band decoding to save memory
static SOSISTAB_NO_OOB: Lazy<bool> = Lazy::new(|| std::env::var("SOSISTAB_NO_OOB").is_ok());

impl OobDecoder {
    /// Create a new OOB decoder that has at most that many entries
    fn new() -> Self {
        let data_frames = SizedCache::with_size(100);
        let parity_space = SizedCache::with_size(10);
        Self {
            data_frames,
            parity_space,
        }
    }

    /// Insert a new data frame.
    fn insert_data(&mut self, frame_no: u64, data: Buff) {
        if *SOSISTAB_NO_OOB {
            return;
        }
        self.data_frames.cache_set(frame_no, data);
    }

    /// Inserts a new parity frame, and attempt to reconstruct.
    fn insert_parity(
        &mut self,
        parity_info: ParitySpaceKey,
        parity_idx: u8,
        parity: Buff,
    ) -> Vec<(u64, Buff)> {
        if *SOSISTAB_NO_OOB {
            return vec![];
        }
        let hash_ref = self
            .parity_space
            .cache_get_or_set_with(parity_info, FxHashMap::default);
        // if 255 is set, this means that the parity is done
        if hash_ref.get(&255).is_some() {
            return vec![];
        }
        hash_ref.insert(parity_idx, parity);

        // now we attempt reconstruction
        let actual_data = {
            let mut toret = Vec::new();
            for i in parity_info.first_data..parity_info.first_data + (parity_info.data_len as u64)
            {
                if let Some(v) = self.data_frames.cache_get(&i) {
                    toret.push((i, v.clone()))
                }
            }
            toret
        };
        if actual_data.len() + hash_ref.len() >= parity_info.data_len as _ {
            hash_ref.insert(255, Buff::new());
            let mut decoder =
                FrameDecoder::new(parity_info.data_len as _, parity_info.parity_len as _);
            // we first insert the data shards.
            for (i, data) in actual_data.iter() {
                if data.len() + 2 > parity_info.pad_size {
                    return vec![];
                }
                let data = pre_encode(data, parity_info.pad_size);
                decoder.decode(&data, (i - parity_info.first_data) as _);
            }
            // make a list of MISSING data ids
            let mut missing_data_seqnos: Vec<_> = (parity_info.first_data
                ..parity_info.first_data + parity_info.data_len as u64)
                .collect();
            for (idx, _) in actual_data.iter() {
                missing_data_seqnos.retain(|v| v != idx);
            }
            // then the parity shards
            for (par_idx, data) in hash_ref {
                if let Some(res) = decoder.decode(
                    data,
                    (parity_info.data_len.saturating_add(*par_idx as u8)) as _,
                ) {
                    assert_eq!(res.len(), missing_data_seqnos.len());
                    return res
                        .into_iter()
                        .zip(missing_data_seqnos.into_iter())
                        .map(|(res, seqno)| (seqno, res))
                        .collect();
                }
            }
        }
        vec![]
    }
}
