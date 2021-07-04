use std::{net::SocketAddr, sync::Arc, time::Duration};

use crate::{runtime, tcp::TcpClientBackhaul, Session, StatsGatherer};

mod inner;

/// Configuration of a client.
#[derive(Clone)]
pub struct ClientConfig {
    pub server_addr: SocketAddr,
    pub server_pk: x25519_dalek::PublicKey,
    pub gather: Arc<StatsGatherer>,
    pub protocol: Protocol,
    pub shard_count: usize,
    pub reset_interval: Option<Duration>,
}

impl ClientConfig {
    /// Creates a new ClientConfig.
    pub fn new(
        protocol: Protocol,
        server_addr: SocketAddr,
        server_pk: x25519_dalek::PublicKey,
        gather: Arc<StatsGatherer>,
    ) -> Self {
        Self {
            server_addr,
            server_pk,
            gather,
            protocol,
            shard_count: 1,
            reset_interval: None,
        }
    }

    /// Builds a Session out of this ClientConfig.
    pub async fn connect(self) -> std::io::Result<Session> {
        let server_addr = self.server_addr;
        let server_pk = self.server_pk;
        inner::connect_custom(inner::LowlevelClientConfig {
            server_addr,
            server_pubkey: server_pk,
            backhaul_gen: match self.protocol {
                Protocol::Tcp => Arc::new(move || {
                    Arc::new(TcpClientBackhaul::new().add_remote_key(server_addr, server_pk))
                }),
                Protocol::Udp => Arc::new(|| {
                    Arc::new(
                        runtime::new_udp_socket_bind("0.0.0.0:0".parse::<SocketAddr>().unwrap())
                            .unwrap(),
                    )
                }),
            },
            num_shards: self.shard_count,
            reset_interval: self.reset_interval,
            gather: self.gather,
        })
        .await
    }
}

/// Underlyiing protocol for a sosistab session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Protocol {
    Tcp,
    Udp,
}

/// Connects to a remote server over UDP.
#[deprecated]
pub async fn connect_udp(
    server_addr: SocketAddr,
    pubkey: x25519_dalek::PublicKey,
    gather: Arc<StatsGatherer>,
) -> std::io::Result<Session> {
    inner::connect_custom(inner::LowlevelClientConfig {
        server_addr,
        server_pubkey: pubkey,
        backhaul_gen: Arc::new(|| {
            Arc::new(
                runtime::new_udp_socket_bind("0.0.0.0:0".parse::<SocketAddr>().unwrap()).unwrap(),
            )
        }),
        num_shards: 4,
        reset_interval: Some(Duration::from_secs(3)),
        gather,
    })
    .await
}

/// Connects to a remote server over UDP.
#[deprecated]
pub async fn connect_tcp(
    server_addr: SocketAddr,
    pubkey: x25519_dalek::PublicKey,
    gather: Arc<StatsGatherer>,
) -> std::io::Result<Session> {
    inner::connect_custom(inner::LowlevelClientConfig {
        server_addr,
        server_pubkey: pubkey,
        backhaul_gen: Arc::new(move || {
            Arc::new(TcpClientBackhaul::new().add_remote_key(server_addr, pubkey))
        }),
        num_shards: 16,
        reset_interval: None,
        gather,
    })
    .await
}
