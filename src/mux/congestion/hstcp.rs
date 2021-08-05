use super::CongestionControl;

/// HSTCP-style congestion control.
pub struct Highspeed {
    cwnd: f64,
}

impl Highspeed {
    /// Creates a new HSTCP instance with the given increment.
    pub fn new() -> Self {
        Self { cwnd: 1.0 }
    }
}

impl CongestionControl for Highspeed {
    fn cwnd(&self) -> usize {
        self.cwnd as usize
    }

    fn mark_ack(&mut self) {
        tracing::trace!("ack => {:.2}", self.cwnd);
        self.cwnd += ((0.23) * self.cwnd.powf(0.4)).max(1.0) / self.cwnd
    }

    fn mark_loss(&mut self) {
        tracing::debug!("loss!!! => {:.2}", self.cwnd);
        self.cwnd = (self.cwnd * 0.5).max(1.0)
    }
}
