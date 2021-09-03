use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use rustc_hash::FxHashMap;

use crate::EmaCalculator;

/// Receive-side loss calculator.
///
/// The basic algorithm is to note "gaps" in packets, then noting them as lost when those gaps are unfilled for a while.
pub struct RecvLossCalc {
    last_seen_seqno: u64,
    good_seqnos: FxHashMap<u64, Instant>,
    gap_seqnos: FxHashMap<u64, Instant>,
    lost_count: f64,
    good_count: f64,
    loss_samples: EmaCalculator,

    // "half-life" of the loss calculation
    window: f64,
    last_loss_update: Instant,
}

impl RecvLossCalc {
    /// Creates a new RecvLossCalc with a given window.
    pub fn new(window: f64) -> Self {
        Self {
            last_seen_seqno: 0,
            good_seqnos: FxHashMap::default(),
            gap_seqnos: FxHashMap::default(),
            lost_count: 0.0,
            good_count: 1.0,
            loss_samples: EmaCalculator::new_unset(0.1),

            window,
            last_loss_update: Instant::now(),
        }
    }

    /// Record a seen seqno
    pub fn record(&mut self, seqno: u64) {
        // first try to fill a gap with this seqno
        if let Some(gap) = self.gap_seqnos.remove(&seqno) {
            self.good_seqnos.insert(seqno, gap);
        } else if seqno > self.last_seen_seqno {
            for missing in (self.last_seen_seqno..seqno).skip(1) {
                self.gap_seqnos.insert(missing, Instant::now());
            }
            self.last_seen_seqno = seqno;
            self.good_seqnos.insert(seqno, Instant::now());
        }
        // prune and calculate loss
        let mut torem = vec![];
        let now = Instant::now();
        for (key, val) in self.good_seqnos.iter() {
            if now.saturating_duration_since(*val) > Duration::from_secs(1) {
                torem.push(*key);
                self.good_count += 1.0;
            } else {
                break;
            }
        }
        for (key, val) in self.gap_seqnos.iter() {
            if now.saturating_duration_since(*val) > Duration::from_secs(1) {
                torem.push(*key);
                tracing::trace!("recv lost {}", key);
                self.lost_count += 1.0;
            } else {
                break;
            }
        }
        for torem in torem {
            self.good_seqnos.remove(&torem);
            self.gap_seqnos.remove(&torem);
        }
        // loss
        let now = Instant::now();
        let loss = self.lost_count / (self.good_count + self.lost_count).max(1.0);
        if now
            .saturating_duration_since(self.last_loss_update)
            .as_secs_f64()
            > self.window
            && self.good_count > 10.0
        {
            tracing::debug!(
                "recording loss {}, ema {}",
                loss,
                self.loss_samples.inverse_cdf(0.1)
            );
            self.loss_samples.update(loss);
            self.last_loss_update = now;
            self.lost_count = 0.0;
            self.good_count = 0.0;
        }
    }

    /// Calculate loss
    pub fn calculate_loss(&mut self) -> f64 {
        self.loss_samples.inverse_cdf(0.1).max(0.0)
    }
}
