use std::ops::DerefMut;

use bincode::Options;
use rand::Rng;
use serde::{Deserialize, Serialize};

use crate::buffer::{Buff, BuffMut};

/// Frame sent as a session-negotiation message. This is always encrypted with the cookie.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum HandshakeFrame {
    /// Frame sent from client to server when opening a connection. This is always globally encrypted.
    ClientHello {
        long_pk: x25519_dalek::PublicKey,
        eph_pk: x25519_dalek::PublicKey,
        version: u64,
    },
    /// Frame sent from server to client to give a cookie for finally opening a connection.
    ServerHello {
        long_pk: x25519_dalek::PublicKey,
        eph_pk: x25519_dalek::PublicKey,
        /// This value includes all the info required to reconstruct a session, encrypted under a secret key only the server knows.
        resume_token: Buff,
    },

    /// Frame sent from client to server to either signal roaming, or complete an initial handshake. This is globally encrypted.
    /// Clients should send a ClientResume every time they suspect that their IP has changed.
    ClientResume {
        resume_token: Buff,
        /// Which shard is this
        shard_id: u8,
    },
}

impl HandshakeFrame {
    pub fn to_bytes(&self) -> Vec<u8> {
        bincode::serialize(self).unwrap()
    }

    pub fn from_bytes(bts: &[u8]) -> anyhow::Result<Self> {
        Ok(bincode::DefaultOptions::new()
            .with_fixint_encoding()
            .allow_trailing_bytes()
            .with_limit(bts.len() as _)
            .deserialize(bts)?)
    }
}

/// Version-2 frame, encrypted with a per-session key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum DataFrameV2 {
    Data {
        /// Strictly incrementing counter of frames. Must never repeat.
        frame_no: u64,
        /// Highest delivered frame
        high_recv_frame_no: u64,
        /// Total delivered frames
        total_recv_frames: u64,
        /// Body
        body: Buff,
    },
    Parity {
        data_frame_first: u64,
        data_count: u8,
        parity_count: u8,
        parity_index: u8,
        pad_size: usize,
        body: Buff,
    },
}

impl DataFrameV2 {
    /// Pads the frame to prepare for encryption.
    pub fn pad(&self, hidden_data: u8) -> Buff {
        let options = bincode::DefaultOptions::new()
            .with_little_endian()
            .with_varint_encoding()
            .allow_trailing_bytes();
        // TODO: padding
        let mut toret = BuffMut::new();
        options.serialize_into(toret.deref_mut(), self).unwrap();
        toret.extend_from_slice(&[hidden_data]);
        let padd_amount = rand::thread_rng().gen_range(0, 10) + (32 - toret.len() % 32);
        toret.extend_from_slice(&vec![0xff; padd_amount]);
        toret.into()
    }

    /// Depads a decrypted frame.
    pub fn depad(bts: &[u8]) -> Option<(Self, u8)> {
        let options = bincode::DefaultOptions::new()
            .with_little_endian()
            .with_varint_encoding()
            .with_limit(bts.len() as _)
            .allow_trailing_bytes();
        let mut ptr = bts;
        let res = options.deserialize_from(&mut ptr).ok()?;
        Some((res, ptr.get(0).copied().unwrap_or(0xff)))
    }
}

/// Version-1 frame sent as an per-session message. This is always encrypted with a per-session key.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataFrameV1 {
    /// Strictly incrementing counter of frames. Must never repeat.
    pub frame_no: u64,
    /// Strictly incrementing counter of runs
    pub run_no: u64,
    /// Run index
    pub run_idx: u8,
    /// Data shards in this run.
    pub data_shards: u8,
    /// Parity shards in this run.
    pub parity_shards: u8,
    /// Index.
    /// Highest delivered frame
    pub high_recv_frame_no: u64,
    /// Total delivered frames
    pub total_recv_frames: u64,
    /// Body.
    pub body: Buff,
}
