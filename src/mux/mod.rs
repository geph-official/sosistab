use crate::{buffer::Buff, runtime, Session};
use smol::channel::{Receiver, Sender};
use std::sync::Arc;
mod congestion;
mod multiplex_actor;
pub mod pkt_trace;
mod relconn;
mod structs;
// pub use congestion::*;
pub use relconn::RelConn;

/// A multiplex session over a sosistab session, implementing both reliable "streams" and unreliable messages.
pub struct Multiplex {
    urel_send: Sender<Buff>,
    urel_recv: Receiver<Buff>,
    conn_open: Sender<(Option<String>, Sender<RelConn>)>,
    conn_accept: Receiver<RelConn>,
    send_session: Sender<Arc<Session>>,
    _task: smol::Task<()>,
}

fn to_ioerror<T: Into<Box<dyn std::error::Error + Send + Sync>>>(val: T) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::ConnectionReset, val)
}

impl Multiplex {
    /// Creates a new multiplexed session
    pub fn new(session: Session) -> Self {
        let (send_session, recv_session) = smol::channel::unbounded();
        let (urel_send, urel_send_recv) = smol::channel::bounded(256);
        let (urel_recv_send, urel_recv) = smol::channel::bounded(4096);
        let (conn_open, conn_open_recv) = smol::channel::unbounded();
        let (conn_accept_send, conn_accept) = smol::channel::bounded(100);
        let session = Arc::new(session);
        send_session.try_send(session).unwrap();
        let _task = runtime::spawn(async move {
            let retval = multiplex_actor::multiplex(
                recv_session,
                urel_send_recv,
                urel_recv_send,
                conn_open_recv,
                conn_accept_send,
            )
            .await;
            tracing::debug!("multiplex actor returned {:?}", retval);
        });
        Multiplex {
            urel_send,
            urel_recv,
            conn_open,
            conn_accept,
            send_session,
            _task,
        }
    }

    /// Sends an unreliable message to the other side
    pub async fn send_urel(&self, msg: impl Into<Buff>) -> std::io::Result<()> {
        self.urel_send.send(msg.into()).await.map_err(to_ioerror)
    }

    pub async fn recv_urel(&self) -> std::io::Result<Buff> {
        self.urel_recv.recv().await.map_err(to_ioerror)
    }

    /// Receive an unreliable message if there is one available.
    pub fn try_recv_urel(&self) -> std::io::Result<Buff> {
        self.urel_recv.try_recv().map_err(to_ioerror)
    }

    // /// Gets a reference to the underlying Session
    // pub async fn get_session(&self) -> impl '_ + Deref<Target = Session> {
    //     self.sess_ref.read().clone()
    // }

    /// Replaces the internal Session. This drops the previous Session, but this is not guaranteed to happen immediately.
    pub async fn replace_session(&self, sess: Session) {
        let sess = Arc::new(sess);
        let _ = self.send_session.try_send(sess);
    }

    /// Open a reliable conn to the other end.
    pub async fn open_conn(&self, additional: Option<String>) -> std::io::Result<RelConn> {
        let (send, recv) = smol::channel::unbounded();
        self.conn_open
            .send((additional.clone(), send))
            .await
            .map_err(to_ioerror)?;
        if let Ok(s) = recv.recv().await {
            Ok(s)
        } else {
            smol::future::pending().await
        }
    }

    /// Accept a reliable conn from the other end.
    pub async fn accept_conn(&self) -> std::io::Result<RelConn> {
        self.conn_accept.recv().await.map_err(to_ioerror)
    }
}
