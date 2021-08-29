use std::{
    cmp::Reverse,
    collections::BinaryHeap,
    sync::Arc,
    time::{Duration, Instant},
};

use futures_intrusive::sync::ManualResetEvent;
use parking_lot::Mutex;
use slab::Slab;

/// Creates a new dejittering pair.
pub fn dejitter<T>() -> (DejitterSend<T>, DejitterRecv<T>) {
    let inner = Arc::new(Mutex::new(DejitterInner {
        queue: Default::default(),
        heap: Default::default(),
        push_time: Instant::now(),
        push_seqno: 0,
        offset: Duration::from_secs(0),
        offset_change: Instant::now(),
        last_pop: 0,
    }));
    let signal = Arc::new(ManualResetEvent::new(true));
    let send = DejitterSend {
        inner: inner.clone(),
        signal: signal.clone(),
    };
    let recv = DejitterRecv { inner, signal };
    (send, recv)
}

/// Sending side of a dejittering pipe.
pub struct DejitterSend<T> {
    inner: Arc<Mutex<DejitterInner<T>>>,
    signal: Arc<ManualResetEvent>,
}

impl<T> DejitterSend<T> {
    /// Sends a value.
    pub fn send(&self, pkt: T, seqno: u64) {
        self.inner.lock().enqueue(pkt, seqno);
        self.signal.set();
    }
}

/// Receiving side of a dejittering pipe.
pub struct DejitterRecv<T> {
    inner: Arc<Mutex<DejitterInner<T>>>,
    signal: Arc<ManualResetEvent>,
}

impl<T> DejitterRecv<T> {
    /// Receives a value.
    pub async fn recv(&self) -> (T, u64) {
        let time = self.inner.lock().next_time();
        smol::Timer::at(time).await;

        loop {
            if let Some(v) = self.inner.lock().dequeue() {
                return v;
            }
            self.signal.wait().await;
            self.signal.reset();
        }
    }
}

struct DejitterInner<T> {
    queue: BinaryHeap<(Reverse<u64>, usize, Instant)>,
    heap: Slab<T>,
    push_seqno: u64,
    push_time: Instant,
    offset: Duration,
    offset_change: Instant,
    last_pop: u64,
}

impl<T> DejitterInner<T> {
    /// Enqueues a packet.
    fn enqueue(&mut self, pkt: T, seqno: u64) {
        let now = Instant::now();
        let pkt = self.heap.insert(pkt);
        self.queue.push((Reverse(seqno), pkt, now));
        if seqno < self.push_seqno {
            let jitter_measurement = now
                .saturating_duration_since(self.push_time)
                .max(Duration::from_millis(10));
            tracing::trace!(
                "jitter {} ({:?}, {:?})",
                self.push_seqno - seqno,
                jitter_measurement,
                self.offset
            );
            if jitter_measurement > self.offset
                || now.saturating_duration_since(self.offset_change).as_secs() > 5
            {
                self.offset = jitter_measurement;
                self.offset_change = now;
            }
            // if jitter_measurement > self.offset {
            //     self.offset = jitter_measurement
            // } else {
            //     self.offset = self.offset * 1023 / 1024 + jitter_measurement / 1024
            // }
            // self.offset = self.offset.max(jitter_measurement);
        }
        self.push_seqno = seqno;
        self.push_time = now;
    }

    /// Calculates the smoothing interval.
    fn next_time(&self) -> Instant {
        self.queue
            .peek()
            .map(|(_, _, time)| *time)
            .unwrap_or_else(Instant::now)
            + self.offset
    }

    /// Dequeues the next packet.
    fn dequeue(&mut self) -> Option<(T, u64)> {
        let (Reverse(seqno), pkt, _) = self.queue.pop()?;
        let pkt = self.heap.remove(pkt);
        if seqno < self.last_pop {
            tracing::warn!(
                "jitter bubbled through: popped {} < {}",
                seqno,
                self.last_pop
            );
        }
        self.last_pop = seqno;
        Some((pkt, seqno))
    }
}
