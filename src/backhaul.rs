use arrayvec::ArrayVec;
use smallvec::smallvec;
use smol::Async;
use std::{
    io,
    net::{SocketAddr, UdpSocket},
    sync::Arc,
};

use crate::{
    buffer::{Buff, BuffMut},
    SVec,
};

/// A trait that represents a datagram backhaul. This presents an interface similar to that of "PacketConn" in Go, and it is used to abstract over different kinds of datagram transports.
#[async_trait::async_trait]
pub(crate) trait Backhaul: Send + Sync {
    /// Sends a datagram
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> io::Result<()>;
    /// Sends many datagrams
    async fn send_to_many(&self, to_send: &[(Buff, SocketAddr)]) -> io::Result<()> {
        for (to_send, dest) in to_send {
            self.send_to(to_send.clone(), *dest).await?
        }
        Ok(())
    }
    /// Waits for the next datagram
    async fn recv_from(&self) -> io::Result<(Buff, SocketAddr)>;
    /// Waits for multiple datagrams.
    async fn recv_from_many(&self) -> io::Result<SVec<(Buff, SocketAddr)>> {
        Ok(smallvec![self.recv_from().await?])
    }
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

    async fn recv_from_many(&self) -> io::Result<SVec<(Buff, SocketAddr)>> {
        let toret = self.haul.recv_from_many().await?;
        for (frag, addr) in toret.iter() {
            (self.on_recv)(frag.len(), *addr);
        }
        Ok(toret)
    }
}

#[async_trait::async_trait]
impl Backhaul for Async<UdpSocket> {
    async fn send_to(&self, to_send: Buff, dest: SocketAddr) -> io::Result<()> {
        if to_send.len() > 1472 {
            tracing::warn!("dropping oversize packet of length {}", to_send.len());
        } else {
            // self.send_to(&to_send, dest).await?;
            self.get_ref().send_to(&to_send, dest)?;
        }
        Ok(())
    }

    async fn recv_from(&self) -> io::Result<(Buff, SocketAddr)> {
        let mut buf = BuffMut::new();
        buf.extend_from_slice(&[0; 2048]);
        let (n, origin) = self.recv_from(&mut buf).await?;
        Ok((buf.freeze().slice(0..n), origin))
    }

    // #[cfg(any(target_os = "linux", target_os = "android"))]
    // async fn send_to_many(&self, to_send: &[(Buff, SocketAddr)]) -> io::Result<()> {
    //     use nix::sys::socket::SendMmsgData;
    //     use nix::sys::socket::{ControlMessage, InetAddr, SockAddr};
    //     use nix::sys::uio::IoVec;
    //     use std::os::unix::prelude::*;
    //     if to_send.len() == 1 {
    //         return Backhaul::send_to(self, to_send[0].0.clone(), to_send[0].1).await;
    //     }
    //     // non-blocking
    //     self.write_with(|sock| {
    //         tracing::trace!("send_to_many({})", to_send.len());
    //         let fd: RawFd = sock.as_raw_fd();
    //         let iov: Vec<[IoVec<&[u8]>; 1]> = to_send
    //             .iter()
    //             .map(|(bts, _)| [IoVec::from_slice(bts)])
    //             .collect();
    //         let control_msgs: Vec<ControlMessage<'static>> = vec![];
    //         let smd: Vec<_> = iov
    //             .iter()
    //             .zip(to_send.iter())
    //             .map(|(iov, (_, addr))| {
    //                 let iov: &[IoVec<&[u8]>] = iov;
    //                 let cmsgs: &[ControlMessage<'static>] = &control_msgs;
    //                 SendMmsgData {
    //                     iov,
    //                     cmsgs,
    //                     addr: Some(SockAddr::new_inet(InetAddr::from_std(addr))),
    //                     _lt: PhantomData::default(),
    //                 }
    //             })
    //             .collect();
    //         nix::sys::socket::sendmmsg(fd, smd.iter(), nix::sys::socket::MsgFlags::empty())
    //             .map_err(to_ioerror)?;
    //         Ok(())
    //     })
    //     .await
    // }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    async fn recv_from_many(&self) -> io::Result<SVec<(Buff, SocketAddr)>> {
        use nix::sys::socket::RecvMmsgData;
        use nix::sys::uio::IoVec;
        use std::os::unix::prelude::*;
        const MAX_LEN: usize = 16;
        self.read_with(|sock| {
            // get fd
            let fd: RawFd = sock.as_raw_fd();
            // create a byte buffer
            let mut byte_slices: ArrayVec<_, 16> = ArrayVec::new();
            for _ in 0..MAX_LEN {
                byte_slices.push({
                    let mut buf = BuffMut::new();
                    buf.resize(2048, 0);
                    buf
                });
            }
            // split into slices
            let response: SVec<(usize, Option<nix::sys::socket::SockAddr>)> = {
                // TODO: WHY DOES THIS NOT WORK WITH SVEC?
                // let byte_slices: SVec<&mut [u8]> = byte_slices.;
                let mut iovs: Vec<[IoVec<&mut [u8]>; 1]> = byte_slices
                    .iter_mut()
                    .map(|bm| [IoVec::from_mut_slice(bm.as_mut_slice())])
                    .collect();

                let mut rmds: Vec<RecvMmsgData<'_, &mut [IoVec<&mut [u8]>]>> = iovs
                    .iter_mut()
                    .map(|iov| {
                        let iov: &mut [IoVec<&mut [u8]>] = iov;
                        RecvMmsgData {
                            iov,
                            cmsg_buffer: None,
                        }
                    })
                    .collect();

                // now do the read
                let response = nix::sys::socket::recvmmsg(
                    fd,
                    rmds.as_mut_slice(),
                    nix::sys::socket::MsgFlags::empty(),
                    None,
                )
                .map_err(to_ioerror)?;
                response.into_iter().map(|v| (v.bytes, v.address)).collect()
            };
            let pkts: SVec<(Buff, SocketAddr)> = response
                .into_iter()
                .zip(byte_slices)
                .filter_map(|((n, src), buff)| {
                    let bts = buff.freeze().slice(0..n);
                    let sockaddr = src?;
                    if let nix::sys::socket::SockAddr::Inet(inetaddr) = sockaddr {
                        Some((bts, inetaddr.to_std()))
                    } else {
                        None
                    }
                })
                .collect();
            // dbg!(pkts.len());
            Ok(pkts)
        })
        .await
    }
}

#[cfg(target_family = "unix")]
fn to_ioerror(err: nix::Error) -> std::io::Error {
    if let Some(errno) = err.as_errno() {
        if errno == nix::errno::EWOULDBLOCK || errno == nix::errno::Errno::EAGAIN {
            return std::io::Error::new(std::io::ErrorKind::WouldBlock, err);
        }
    }
    std::io::Error::new(std::io::ErrorKind::ConnectionAborted, err)
}
