use std::{
    cmp::Reverse,
    time::{Duration, Instant},
};

use ordered_float::OrderedFloat;

use crate::{EmaCalculator, MinQueue};

pub struct RttCalculator {
    inner: EmaCalculator,

    min_rtt: Duration,
    rtt_update_time: Instant,
}

impl Default for RttCalculator {
    fn default() -> Self {
        RttCalculator {
            inner: EmaCalculator::new(0.5, 0.01),
            min_rtt: Duration::from_millis(500),
            rtt_update_time: Instant::now(),
        }
    }
}

impl RttCalculator {
    pub fn record_sample(&mut self, sample: Duration) {
        let now = Instant::now();
        if sample < self.min_rtt
            || now
                .saturating_duration_since(self.rtt_update_time)
                .as_millis()
                > 3000
        {
            self.min_rtt = sample;
            self.rtt_update_time = now;
        }
        self.inner.update(sample.as_secs_f64())
    }

    pub fn rto(&self) -> Duration {
        Duration::from_secs_f64(self.inner.inverse_cdf(0.99) + 0.25)
    }

    // pub fn srtt(&self) -> Duration {
    //     Duration::from_secs_f64(self.inner.mean())
    // }

    pub fn rtt_var(&self) -> Duration {
        Duration::from_secs_f64(self.inner.inverse_cdf(0.99) - self.inner.inverse_cdf(0.01))
    }

    pub fn min_rtt(&self) -> Duration {
        self.min_rtt
    }
}

pub struct BwCalculator {
    delivered: u64,
    delivered_time: Instant,
    delivery_max_filter: MinQueue<Reverse<(OrderedFloat<f64>, Instant)>>,
}

impl Default for BwCalculator {
    fn default() -> Self {
        Self {
            delivered: 0,
            delivered_time: Instant::now(),
            delivery_max_filter: MinQueue::new(),
        }
    }
}

impl BwCalculator {
    /// On ack
    pub fn on_ack(&mut self, packet_delivered: u64, packet_delivered_time: Instant) {
        self.delivered += 1;
        self.delivered_time = Instant::now();
        let delivery_rate = (self.delivered - packet_delivered) as f64
            / (self.delivered_time - packet_delivered_time).as_secs_f64();
        self.delivery_max_filter
            .push_back(Reverse((delivery_rate.into(), self.delivered_time)));
        while self
            .delivery_max_filter
            .peek_front()
            .copied()
            .map(|f| f.0 .1.elapsed().as_secs_f64() > 2.0)
            .unwrap_or_default()
        {
            self.delivery_max_filter.pop_front();
        }
        // tracing::warn!("current rate is {}", self.delivery_rate());
    }

    /// Gets the current delivery rate
    pub fn delivery_rate(&self) -> f64 {
        self.delivery_max_filter
            .min()
            .copied()
            .map(|f| f.0 .0 .0)
            .unwrap_or_default()
    }

    /// Gets the current delivered packets
    pub fn delivered(&self) -> u64 {
        self.delivered
    }

    /// Gets the current delivered time
    pub fn delivered_time(&self) -> Instant {
        self.delivered_time
    }
}
