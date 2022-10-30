use smol::Async;
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::Arc,
};

use crate::buffer::{Buff, BuffMut};

/// A trait that represents a datagram backhaul. This presents an interface similar to that of "PacketConn" in Go, and it is used to abstract over different kinds of datagram transports.
#[async_trait::async_trait]
pub(crate) trait Backhaul: Send + Sync {
    /// Sends a datagram
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> io::Result<()>;

    /// Waits for the next datagram
    async fn recv_from(&self) -> io::Result<(Buff, SocketAddr)>;
}

/// A structure that wraps a Backhaul with statistics.
pub(crate) struct StatsBackhaul<B: Backhaul + 'static> {
    haul: Arc<B>,
    on_recv: Box<dyn Fn(usize, SocketAddr) + Send + Sync>,
    on_send: Box<dyn Fn(usize, SocketAddr) + Send + Sync>,
}

impl<B: Backhaul + 'static> StatsBackhaul<B> {
    pub fn new(
        haul: B,
        on_recv: impl Fn(usize, SocketAddr) + 'static + Send + Sync,
        on_send: impl Fn(usize, SocketAddr) + 'static + Send + Sync,
    ) -> Self {
        let haul = Arc::new(haul);
        Self {
            haul,
            on_recv: Box::new(on_recv),
            on_send: Box::new(on_send),
        }
    }
}

#[async_trait::async_trait]
impl<B: Backhaul> Backhaul for StatsBackhaul<B> {
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> io::Result<()> {
        (self.on_send)(to_send.len(), dest);
        self.haul.send_to(to_send, dest).await
    }

    async fn recv_from(&self) -> io::Result<(Buff, SocketAddr)> {
        let (bts, addr) = self.haul.recv_from().await?;
        (self.on_recv)(bts.len(), addr);
        Ok((bts, addr))
    }
}

#[async_trait::async_trait]
#[cfg(target_os = "linux")]
impl Backhaul for fastudp::FastUdpSocket {
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> io::Result<()> {
        if to_send.len() > 1472 {
            tracing::warn!("dropping oversize packet of length {}", to_send.len());
        } else {
            self.send_to(&to_send, dest).await?;
            // self.get_ref().send_to(&to_send, dest)?;
        }
        Ok(())
    }

    async fn recv_from(&self) -> io::Result<(Buff, SocketAddr)> {
        let mut buf = BuffMut::new();
        buf.extend_from_slice(&[0; 2048]);
        let (n, origin) = self.recv_from(&mut buf).await?;
        Ok((buf.freeze().slice(0..n), origin))
    }
}

#[async_trait::async_trait]
impl Backhaul for Async<UdpSocket> {
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> io::Result<()> {
        if to_send.len() > 1472 {
            tracing::warn!("dropping oversize packet of length {}", to_send.len());
        } else {
            self.send_to(&to_send, dest).await?;
            // self.get_ref().send_to(&to_send, dest)?;
        }
        Ok(())
    }

    async fn recv_from(&self) -> io::Result<(Buff, SocketAddr)> {
        let mut buf = BuffMut::new();
        buf.extend_from_slice(&[0; 2048]);
        let (n, origin) = self.recv_from(&mut buf).await?;
        Ok((buf.freeze().slice(0..n), origin))
    }
}

#[cfg(target_family = "unix")]
fn to_ioerror(err: nix::Error) -> std::io::Error {
    if err == nix::errno::Errno::EWOULDBLOCK || err == nix::errno::Errno::EAGAIN {
        return std::io::Error::new(std::io::ErrorKind::WouldBlock, err);
    }

    std::io::Error::new(std::io::ErrorKind::ConnectionAborted, err)
}
