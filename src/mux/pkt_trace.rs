use std::time::Instant;

use super::structs::{Message, RelKind, Seqno};
use once_cell::sync::{Lazy, OnceCell};
use serde::Serialize;

/// Packet-tracing sink
static PACKET_TRACE_SINK: OnceCell<Box<dyn Fn(String) + Sync + Send>> = OnceCell::new();

/// Initialize the packet-trace sink.
pub fn init_packet_tracing(per_line: impl Fn(String) + Send + Sync + 'static) {
    PACKET_TRACE_SINK
        .set(Box::new(per_line))
        .ok()
        .expect("already initialized");
}

/// A packet-tracing context.
#[derive(Clone, Debug)]
pub struct PktTraceCtx {
    mux_uniqid: u64,
}

static START_TIME: Lazy<Instant> = Lazy::new(Instant::now);

impl PktTraceCtx {
    /// Creates a new, unique context.
    pub fn new_random() -> Self {
        let mux_uniqid = rand::random();
        Self { mux_uniqid }
    }
    /// Traces a packet.
    pub fn trace_pkt(&self, pkt: &Message, direction: bool) {
        if let Some(cb) = PACKET_TRACE_SINK.get() {
            let timestamp = START_TIME.elapsed().as_secs_f64();
            let evt = match pkt {
                Message::Empty => PktTraceEvt::Empty {
                    mux_id: self.mux_uniqid,
                    timestamp,
                    direction,
                },
                Message::Rel {
                    kind,
                    stream_id,
                    seqno,
                    payload,
                } => PktTraceEvt::Rel {
                    mux_id: self.mux_uniqid,
                    timestamp,
                    direction,
                    kind: *kind,
                    stream_id: *stream_id,
                    seqno: *seqno,
                    body_length: payload.len(),
                },
                Message::Urel(buff) => PktTraceEvt::Urel {
                    mux_id: self.mux_uniqid,
                    timestamp,
                    direction,
                    body_length: buff.len(),
                },
            };
            let line = serde_json::to_string(&evt).unwrap();
            tracing::trace!("trace_pkt: {}", line);
            cb(line);
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "type")]
enum PktTraceEvt {
    Urel {
        mux_id: u64,
        timestamp: f64,
        direction: bool,
        body_length: usize,
    },
    Rel {
        mux_id: u64,
        timestamp: f64,
        direction: bool,
        kind: RelKind,
        stream_id: u16,
        seqno: Seqno,
        body_length: usize,
    },
    Empty {
        mux_id: u64,
        timestamp: f64,
        direction: bool,
    },
}
