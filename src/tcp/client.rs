use dashmap::DashMap;
use rustc_hash::FxHashMap;
use smol::channel::{Receiver, Sender};
use smol::prelude::*;
use std::{
    collections::VecDeque,
    convert::TryInto,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, SystemTime},
};

use crate::{
    buffer::Buff,
    crypt::{triple_ecdh, Cookie, NgAead},
    protocol::HandshakeFrame,
    runtime, Backhaul, Connector,
};
use anyhow::Context;
use smol_timeout::TimeoutExt;

use super::{
    read_encrypted, write_encrypted, DynAsyncRead, DynAsyncWrite, ObfsTcp, CONN_LIFETIME,
    TCP_DN_KEY, TCP_UP_KEY,
};

/// A TCP-based backhaul, client-side.
pub struct TcpClientBackhaul {
    dest_to_key: FxHashMap<SocketAddr, x25519_dalek::PublicKey>,
    conn_pool: DashMap<SocketAddr, VecDeque<(ObfsTcp, SystemTime)>>,
    fake_addr: u128,
    incoming: Receiver<(Buff, SocketAddr)>,
    send_incoming: Sender<(Buff, SocketAddr)>,

    connect: Connector,
    tls: bool,
}

impl TcpClientBackhaul {
    /// Creates a new TCP client backhaul.
    pub fn new(connect: Option<Connector>, tls: bool) -> Self {
        // dummy here
        let (send_incoming, incoming) = smol::channel::unbounded();
        let fake_addr = rand::random::<u128>();
        Self {
            dest_to_key: Default::default(),
            conn_pool: Default::default(),
            fake_addr,
            incoming,
            send_incoming,
            connect: connect.unwrap_or_else(move || {
                Arc::new(move |addr| smol::net::TcpStream::connect(addr).boxed())
            }),
            tls,
        }
    }

    /// Adds a binding.
    pub fn add_remote_key(mut self, addr: SocketAddr, key: x25519_dalek::PublicKey) -> Self {
        self.dest_to_key.insert(addr, key);
        self
    }

    /// Gets a connection out of the pool of an address.
    fn get_conn_pooled(&self, addr: SocketAddr) -> Option<(ObfsTcp, SystemTime)> {
        let mut pool = self.conn_pool.entry(addr).or_default();
        while let Some((conn, time)) = pool.pop_front() {
            if let Ok(age) = time.elapsed() {
                if age < CONN_LIFETIME {
                    return Some((conn, time));
                }
            }
        }
        None
    }

    /// Puts a connection back into the pool of an address.
    fn put_conn(&self, addr: SocketAddr, stream: ObfsTcp, time: SystemTime) {
        let mut pool = self.conn_pool.entry(addr).or_default();
        pool.push_back((stream, time));
    }

    /// Opens a connection or gets a connection from the pool.
    async fn get_conn(&self, addr: SocketAddr) -> anyhow::Result<(ObfsTcp, SystemTime)> {
        if let Some(pooled) = self.get_conn_pooled(addr) {
            Ok(pooled)
        } else {
            let my_long_sk = x25519_dalek::StaticSecret::new(&mut rand::thread_rng());
            let my_eph_sk = x25519_dalek::StaticSecret::new(&mut rand::thread_rng());

            let pubkey = *self
                .dest_to_key
                .get(&addr)
                .ok_or_else(|| anyhow::anyhow!("remote address doesn't have a public key"))?;
            let cookie = Cookie::new(pubkey);
            // first connect
            let (mut remote_write, mut remote_read): (DynAsyncWrite, DynAsyncRead) = if self.tls {
                let tcp = (self.connect)(addr).await?;
                let connector = async_native_tls::TlsConnector::new()
                    .danger_accept_invalid_certs(true)
                    .danger_accept_invalid_hostnames(true)
                    .use_sni(false);
                let tls = async_dup::Arc::new(async_dup::Mutex::new(
                    connector.connect("example.com", tcp).await?,
                ));
                eprintln!("*** TLS ESTABLISHED YAAAY!!!! ***");
                (Box::new(tls.clone()), Box::new(tls))
            } else {
                let tcp = (self.connect)(addr).await?;
                (Box::new(tcp.clone()), Box::new(tcp))
            };

            // then we send a hello
            let init_c2s = cookie.generate_c2s().next().unwrap();
            let init_s2c = cookie.generate_s2c().next().unwrap();
            let init_up_key = blake3::keyed_hash(TCP_UP_KEY, &init_c2s);
            let init_enc = NgAead::new(init_up_key.as_bytes());
            let to_send = HandshakeFrame::ClientHello {
                long_pk: (&my_long_sk).into(),
                eph_pk: (&my_eph_sk).into(),
                version: 3,
            };
            let mut to_send = to_send.to_bytes();
            let random_padding = vec![0u8; rand::random::<usize>() % 1024];
            to_send.extend_from_slice(&random_padding);
            let mut buf = vec![];
            write_encrypted(init_enc, &to_send, &mut buf).await?;
            remote_write.write_all(&buf).await?;
            // now we wait for a response
            let init_dn_key = blake3::keyed_hash(TCP_DN_KEY, &init_s2c);
            let init_dec = NgAead::new(init_dn_key.as_bytes());
            let raw_response = read_encrypted(init_dec, &mut remote_read)
                .await
                .context("can't read response from server")?;
            let actual_response = HandshakeFrame::from_bytes(&raw_response)?;
            if let HandshakeFrame::ServerHello {
                long_pk,
                eph_pk,
                resume_token: _,
            } = actual_response
            {
                let shared_sec = triple_ecdh(&my_long_sk, &my_eph_sk, &long_pk, &eph_pk);
                let connection = ObfsTcp::new(shared_sec, false, remote_write, remote_read);
                connection.write(&self.fake_addr.to_be_bytes()).await?;
                let down_conn = connection.clone();
                let send_incoming = self.send_incoming.clone();
                // spawn a thread that reads from the connection
                runtime::spawn(async move {
                    let mut buffer = [0u8; 65536];
                    let main = async {
                        loop {
                            down_conn.read_exact(&mut buffer[..2]).await?;
                            let length =
                                u16::from_be_bytes((&buffer[..2]).try_into().unwrap()) as usize;
                            down_conn.read_exact(&mut buffer[..length]).await?;
                            let _ = send_incoming
                                .try_send((Buff::copy_from_slice(&buffer[..length]), addr));
                        }
                    };
                    let _: anyhow::Result<()> = main
                        .or(async {
                            smol::Timer::after(CONN_LIFETIME).await;
                            Ok(())
                        })
                        .await;
                })
                .detach();

                Ok((connection, SystemTime::now()))
            } else {
                anyhow::bail!("server sent unrecognizable message")
            }
        }
    }
}

#[async_trait::async_trait]
impl Backhaul for TcpClientBackhaul {
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> std::io::Result<()> {
        if to_send.len() > 2048 {
            tracing::warn!("refusing to send packet of length {}", to_send.len());
            return Ok(());
        }

        let mut buf = [0u8; 4096];
        buf[0..2].copy_from_slice(&(to_send.len() as u16).to_be_bytes());
        buf[2..to_send.len() + 2].copy_from_slice(&to_send);
        let res: anyhow::Result<()> = async {
            let (conn, time) = self
                .get_conn(dest)
                .timeout(Duration::from_secs(10))
                .await
                .ok_or_else(|| anyhow::anyhow!("timeout"))??;
            conn.write(&buf[..to_send.len() + 2])
                .or(async {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "TCP write buffer is full, throwing connection away",
                    ))
                })
                .await?;
            self.put_conn(dest, conn, time);
            Ok(())
        }
        .await;

        if let Err(err) = res {
            tracing::debug!("error in TcpClientBackhaul: {:?}", err);
        }

        Ok(())
    }

    async fn recv_from(&self) -> std::io::Result<(Buff, SocketAddr)> {
        Ok(self.incoming.recv().await.unwrap())
    }
}
