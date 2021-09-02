use std::time::{Duration, Instant};

use crate::EmaCalculator;

const MAX_MEASUREMENTS: usize = 32;

pub struct RttCalculator {
    inner: EmaCalculator,
}

impl Default for RttCalculator {
    fn default() -> Self {
        RttCalculator {
            inner: EmaCalculator::new(0.5, 0.01),
        }
    }
}

impl RttCalculator {
    pub fn record_sample(&mut self, sample: Duration) {
        self.inner.update(sample.as_secs_f64())
    }

    pub fn rto(&self) -> Duration {
        Duration::from_secs_f64(self.inner.inverse_cdf(0.9999) + 0.05)
    }

    // pub fn srtt(&self) -> Duration {
    //     Duration::from_secs_f64(self.inner.mean())
    // }

    // pub fn rtt_var(&self) -> Duration {
    //     Duration::from_millis(
    //         *self.rtt_measurements.last().unwrap() - *self.rtt_measurements.first().unwrap(),
    //     )
    // }

    pub fn min_rtt(&self) -> Duration {
        Duration::from_secs_f64(self.inner.inverse_cdf(0.1).max(0.0))
    }
}
