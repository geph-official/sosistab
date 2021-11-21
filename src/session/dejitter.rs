use std::{
    cmp::Reverse,
    collections::{BinaryHeap, VecDeque},
    time::{Duration, Instant},
};

use slab::Slab;
use smol::{channel::Receiver, future::FutureExt};

use crate::EmaCalculator;

enum DejitterEvt<T> {
    NewInject(Result<(T, u64), smol::channel::RecvError>),
    Timeout,
}

pub struct DejitterRecv<T> {
    injector: Receiver<(T, u64)>,
    // all of the actual packets, "deduplicated"
    packets: Slab<T>,
    // all the arrival times
    arrivals: VecDeque<Instant>,
    // priority queue in sequence-number order
    order: BinaryHeap<(Reverse<u64>, usize)>,
    // timer
    timer: smol::Timer,
    // inversion vars
    last_inject: Option<(Instant, u64)>,
    max_inversion: EmaCalculator,
    last_popped: u64,
}

impl<T> DejitterRecv<T> {
    /// Wraps a receiver
    pub fn new(injector: Receiver<(T, u64)>) -> Self {
        Self {
            injector,
            packets: Default::default(),
            arrivals: Default::default(),
            order: Default::default(),
            timer: smol::Timer::at(Instant::now()),
            last_inject: None,
            max_inversion: EmaCalculator::new(0.001, 0.001),
            last_popped: 0,
        }
    }

    /// Receives the next packet.
    pub async fn recv(&mut self) -> Result<(T, u64), smol::channel::RecvError> {
        loop {
            // if consecutive, then directly return
            if self.order.peek().map(|f| f.0 .0) == Some(self.last_popped + 1) {
                return Ok(self.pop_local().expect("must pop"));
            }
            let empty = self.arrivals.is_empty();
            if !empty {
                let offset = Duration::from_millis(20);
                self.timer.set_at(self.arrivals[0] + offset)
            }
            let injector = self.injector.clone();
            let new_inject_fut = async { DejitterEvt::NewInject(injector.recv().await) };
            let timeout_fut = async {
                if empty {
                    smol::future::pending::<DejitterEvt<T>>().await
                } else {
                    (&mut self.timer).await;
                    DejitterEvt::Timeout
                }
            };
            match new_inject_fut.race(timeout_fut).await {
                DejitterEvt::Timeout => return Ok(self.pop_local().expect("must succeed here")),
                DejitterEvt::NewInject(rr) => {
                    let rr = rr?;
                    self.push_local(rr.0, rr.1)
                }
            }
        }
    }

    /// Pops out something already in the queue
    fn pop_local(&mut self) -> Option<(T, u64)> {
        assert!(self.arrivals.len() == self.order.len());
        let (Reverse(seqno), idx) = self.order.pop()?;
        self.arrivals.pop_front();
        self.last_popped = seqno;
        Some((self.packets.remove(idx), seqno))
    }

    /// Pushes something into the queue
    fn push_local(&mut self, packet: T, seqno: u64) {
        let now = Instant::now();
        if let Some((last, last_seqno)) = self.last_inject.replace((now, seqno)) {
            if last_seqno > seqno {
                let current_inversion = now.saturating_duration_since(last);
                self.max_inversion.update(current_inversion.as_secs_f64());
            }
        }
        self.arrivals.push_back(Instant::now());
        let idx = self.packets.insert(packet);
        self.order.push((Reverse(seqno), idx))
    }
}
