use super::CongestionControl;

/// Classic, Reno-style congestion control.
pub struct Reno {
    cwnd: f64,
    incr: f64,
}

impl Reno {
    /// Creates a new Reno instance with the given increment.
    pub fn new(incr: usize) -> Self {
        Self {
            cwnd: 1.0,
            incr: incr as f64,
        }
    }
}

impl CongestionControl for Reno {
    fn cwnd(&self) -> usize {
        self.cwnd as usize
    }

    fn mark_ack(&mut self) {
        tracing::trace!("ack => {:.2}", self.cwnd);
        self.cwnd += self.incr / self.cwnd
    }

    fn mark_loss(&mut self) {
        tracing::debug!("loss!!! => {:.2}", self.cwnd);
        self.cwnd = (self.cwnd * 0.5).max(1.0)
    }
}
