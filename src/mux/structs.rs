use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::buffer::Buff;

/// A sequence number.
pub type Seqno = u64;
/// A message.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Message {
    Urel(Buff),
    Rel {
        kind: RelKind,
        stream_id: u16,
        seqno: Seqno,
        payload: Buff,
    },
    Empty,
}

#[derive(Copy, Clone, Debug, Serialize, Deserialize, Eq, PartialEq)]
pub enum RelKind {
    Syn,
    SynAck,
    Data,
    DataAck,
    Fin,
    FinAck,
    Rst,
}

#[derive(Clone)]
pub struct Reorderer<T: Clone> {
    pkts: FxHashMap<Seqno, T>,
    min: Seqno,
}

impl<T: Clone> Default for Reorderer<T> {
    fn default() -> Self {
        Reorderer {
            pkts: FxHashMap::default(),
            min: 0,
        }
    }
}
impl<T: Clone> Reorderer<T> {
    /// Inserts an item into the reorderer. Returns true iff the item is accepted or has been accepted in the past.
    pub fn insert(&mut self, seq: Seqno, item: T) -> bool {
        if seq >= self.min && seq <= self.min + 20000 {
            if self.pkts.insert(seq, item).is_some() {
                tracing::debug!("spurious retransmission of {} received", seq);
            }
            true
        } else {
            tracing::debug!("rejecting (seq={}, min={})", seq, self.min);
            // if less than min, we still accept
            seq < self.min
        }
    }
    pub fn take(&mut self) -> Vec<T> {
        let mut output = Vec::with_capacity(self.pkts.len());
        for idx in self.min.. {
            if let Some(item) = self.pkts.remove(&idx) {
                output.push(item.clone());
                self.min = idx + 1;
            } else {
                break;
            }
        }
        output
    }
}
