use crate::mux::structs::*;
use std::{
    collections::{btree_map::Entry, BTreeMap},
    time::{Duration, Instant},
};

use self::calc::RttCalculator;

mod calc;

#[derive(Debug, Clone)]
/// An element of Inflight.
pub struct InflightEntry {
    seqno: Seqno,
    send_time: Instant,
    pub retrans: u64,
    pub payload: Message,

    retrans_time: Instant,

    known_lost: bool,
}

/// A data structure that tracks in-flight packets.
pub struct Inflight {
    segments: BTreeMap<Seqno, InflightEntry>,
    rtos: BTreeMap<Instant, Vec<Seqno>>,
    lost_count: usize,
    rtt: RttCalculator,
    max_inversion: Duration,
    max_acked_sendtime: Instant,
}

impl Inflight {
    /// Creates a new Inflight.
    pub fn new() -> Self {
        Inflight {
            segments: Default::default(),
            rtos: Default::default(),
            rtt: Default::default(),
            lost_count: 0,
            max_inversion: Duration::from_millis(1),
            max_acked_sendtime: Instant::now(),
        }
    }

    pub fn unacked(&self) -> usize {
        self.segments.len()
    }

    pub fn inflight(&self) -> usize {
        // all segments that are still in flight
        self.segments.len() - self.lost_count
    }

    pub fn lost_count(&self) -> usize {
        self.lost_count
    }

    // pub fn srtt(&self) -> Duration {
    //     self.rtt.srtt()
    // }

    // pub fn rtt_var(&self) -> Duration {
    //     self.rtt.rtt_var()
    // }

    pub fn min_rtt(&self) -> Duration {
        self.rtt.min_rtt()
    }

    pub fn rto(&self) -> Duration {
        self.rtt.rto()
    }

    /// Mark all inflight packets less than a certain sequence number as acknowledged.
    pub fn mark_acked_lt(&mut self, seqno: Seqno) -> usize {
        let mut to_remove = vec![];
        for (k, _) in self.segments.iter() {
            if *k < seqno {
                to_remove.push(*k);
            } else {
                // we can rely on iteration order
                break;
            }
        }
        let mut sum = 0;
        for seqno in to_remove {
            sum += if self.mark_acked(seqno) { 1 } else { 0 };
        }
        sum
    }

    /// Marks a particular inflight packet as acknowledged. Returns whether or not there was actually such an inflight packet.
    pub fn mark_acked(&mut self, acked_seqno: Seqno) -> bool {
        let now = Instant::now();

        if let Some(acked_seg) = self.segments.remove(&acked_seqno) {
            if acked_seg.retrans == 0 {
                self.max_acked_sendtime = acked_seg.send_time.max(self.max_acked_sendtime);
                self.rtt
                    .record_sample(now.saturating_duration_since(acked_seg.send_time));
                self.max_inversion = self
                    .max_acked_sendtime
                    .saturating_duration_since(acked_seg.send_time)
                    .max(self.max_inversion);
            }
            // remove from rtos
            let rto_entry = self.rtos.entry(acked_seg.retrans_time);
            if let Entry::Occupied(mut o) = rto_entry {
                o.get_mut().retain(|v| *v != acked_seqno);
                if o.get().is_empty() {
                    o.remove();
                }
            } else {
                panic!("shouldn't happen")
            }
            if acked_seg.known_lost {
                self.lost_count -= 1;
            }
            // // mark as lost everything below
            // let mark_as_lost: Vec<u64> = self
            //     .segments
            //     .keys()
            //     .take_while(|f| **f < acked_seqno)
            //     .copied()
            //     .collect();
            // let now = Instant::now();
            // for seqno in mark_as_lost {
            //     let seg = self.segments.get_mut(&seqno).unwrap();
            //     // if send time was in the past far enough, retransmit
            //     if seg.retrans == 0
            //         && seg.retrans_time + self.max_inversion * 2 <= acked_seg.retrans_time
            //         && seg.retrans_time > now
            //     {
            //         tracing::debug!(
            //             "EARLY retransmit for lost segment {} due to ack of {}",
            //             seqno,
            //             acked_seqno
            //         );
            //         let old_retrans_time = std::mem::replace(&mut seg.retrans_time, now);
            //         self.remove_rto(old_retrans_time, seqno);
            //         self.rtos.entry(now).or_default().push(seqno);
            //     }
            // }
            true
        } else {
            false
        }
    }

    /// Marks a particular packet as known to be lost. Does not immediately retransmit it yet!
    pub fn mark_lost(&mut self, seqno: Seqno) -> bool {
        if let Some(seg) = self.segments.get_mut(&seqno) {
            let was_lost = std::mem::replace(&mut seg.known_lost, true);
            let retrans_time = seg.retrans_time;
            self.remove_rto(retrans_time, seqno);
            if !was_lost {
                self.lost_count += 1;
            }
            true
        } else {
            false
        }
    }

    /// Inserts a packet to the inflight.
    pub fn insert(&mut self, seqno: Seqno, msg: Message) {
        let now = Instant::now();
        let rto_duration = self.rtt.rto();
        let rto = now + rto_duration;
        let prev = self.segments.insert(
            seqno,
            InflightEntry {
                seqno,
                send_time: now,
                payload: msg,
                retrans: 0,
                retrans_time: rto,
                known_lost: false,
            },
        );
        assert!(prev.is_none());
        // we insert into RTOs.
        self.rtos.entry(rto).or_default().push(seqno);
    }

    /// Returns the retransmission time of the first possibly retransmitted packet, as well as its seqno. This skips all known-lost packets.
    pub fn first_rto(&self) -> Option<(Seqno, Instant)> {
        self.rtos
            .iter()
            .next()
            .map(|(instant, seqno)| (seqno[0], *instant))
    }
    /// Retransmits a particular seqno, clearing the "known lost" flag on the way.
    pub fn retransmit(&mut self, seqno: Seqno) -> Option<Message> {
        // tracing::d!("retransmit {}", seqno);
        let rto = self.rtt.rto();
        let (payload, old_retrans, new_retrans) = {
            let entry = self.segments.get_mut(&seqno);
            entry.map(|entry| {
                let old_retrans = entry.retrans_time;
                entry.retrans += 1;
                entry.retrans_time =
                    Instant::now() + rto.mul_f64(2.0f64.powi(entry.retrans as i32));
                entry.known_lost = false;
                (entry.payload.clone(), old_retrans, entry.retrans_time)
            })?
        };
        let rto_entry = self.rtos.entry(old_retrans);
        if let Entry::Occupied(mut o) = rto_entry {
            o.get_mut().retain(|v| *v != seqno);
            if o.get().is_empty() {
                o.remove();
            }
        }
        self.rtos.entry(new_retrans).or_default().push(seqno);
        self.lost_count -= 1;

        Some(payload)
    }

    fn remove_rto(&mut self, retrans_time: Instant, seqno: Seqno) {
        let rto_entry = self.rtos.entry(retrans_time);
        if let Entry::Occupied(mut o) = rto_entry {
            o.get_mut().retain(|v| *v != seqno);
            if o.get().is_empty() {
                o.remove();
            }
        } else {
            panic!("shouldn't happen")
        }
    }
}
