use dashmap::DashMap;
use rand::prelude::*;
use smol::channel::{Receiver, Sender};
use smol::prelude::*;
use std::{ops::DerefMut, sync::Arc, time::Duration};

use crate::{
    buffer::{Buff, BuffMut},
    mux::pkt_trace::PktTraceCtx,
    runtime, safe_deserialize, RelConn, Session,
};

use super::{
    relconn::{RelConnBack, RelConnState},
    structs::{Message, RelKind},
};

pub async fn multiplex(
    recv_session: Receiver<Session>,
    urel_send_recv: Receiver<Buff>,
    urel_recv_send: Sender<Buff>,
    conn_open_recv: Receiver<(Option<String>, Sender<RelConn>)>,
    conn_accept_send: Sender<RelConn>,
) -> anyhow::Result<()> {
    let trace_ctx = PktTraceCtx::new_random();
    let conn_tab = Arc::new(ConnTable::default());
    let (glob_send, glob_recv) = smol::channel::bounded(1000);
    let (dead_send, dead_recv) = smol::channel::unbounded();

    // Reap death
    let reap_dead = {
        let dead_send = dead_send.clone();
        move |id: u16| {
            tracing::debug!("reaper received {}", id);
            runtime::spawn(async move {
                smol::Timer::after(Duration::from_secs(30)).await;
                tracing::debug!("reaper executed {}", id);
                let _ = dead_send.try_send(id);
            })
            .detach()
        }
    };

    let mut session = recv_session.recv().await?;

    // enum of possible events
    enum Event {
        SessionReplace(Session),
        RecvMsg(Message),
        SendMsg(Message),
        ConnOpen(Option<String>, Sender<RelConn>),
        Dead(u16),
    }

    loop {
        // fires on session replacement
        let sess_replace = async {
            let new_session = recv_session.recv().await?;
            Ok::<_, anyhow::Error>(Event::SessionReplace(new_session))
        };
        // fires on receiving messages
        let recv_msg = async {
            let msg = session.recv_bytes().await?;
            let msg = safe_deserialize(&msg);
            if let Ok(msg) = msg {
                Ok::<_, anyhow::Error>(Event::RecvMsg(msg))
            } else {
                tracing::trace!("unrecognizable message from sess");
                // In this case, we echo back an empty packet.
                Ok(Event::SendMsg(Message::Empty))
            }
        };
        // fires on sending urel
        let send_urel = async {
            let msg = urel_send_recv.recv().await?;
            Ok(Event::SendMsg(Message::Urel(msg)))
        };
        // fires on sending messages
        let send_msg = async {
            let to_send = glob_recv.recv().await?;
            Ok::<_, anyhow::Error>(Event::SendMsg(to_send))
        };
        // fires on stream open events
        let conn_open = async {
            let (additional_data, result_chan) = conn_open_recv.recv().await?;
            Ok::<_, anyhow::Error>(Event::ConnOpen(additional_data, result_chan))
        };
        // fires on death
        let death = async {
            let res = dead_recv.recv().await?;
            Ok::<_, anyhow::Error>(Event::Dead(res))
        };
        // match on the event
        match conn_open
            .or(recv_msg.or(send_urel.or(send_msg.or(sess_replace.or(death)))))
            .await?
        {
            Event::SessionReplace(new_sess) => session = new_sess,
            Event::Dead(id) => conn_tab.del_stream(id),
            Event::ConnOpen(additional_data, result_chan) => {
                let conn_tab = conn_tab.clone();
                let glob_send = glob_send.clone();
                let reap_dead = reap_dead.clone();
                runtime::spawn(async move {
                    let stream_id = {
                        let stream_id = conn_tab.find_id();
                        if let Some(stream_id) = stream_id {
                            let (send_sig, recv_sig) = smol::channel::bounded(1);
                            let (conn, conn_back) = RelConn::new(
                                RelConnState::SynSent {
                                    stream_id,
                                    tries: 0,
                                    result: send_sig,
                                },
                                glob_send.clone(),
                                move || reap_dead(stream_id),
                                additional_data.clone(),
                            );
                            runtime::spawn(async move {
                                recv_sig.recv().await.ok()?;
                                result_chan.send(conn).await.ok()?;
                                Some(())
                            })
                            .detach();
                            conn_tab.set_stream(stream_id, conn_back);
                            stream_id
                        } else {
                            return;
                        }
                    };
                    tracing::trace!("conn open send {}", stream_id);
                    let _ = glob_send.try_send(Message::Rel {
                        kind: RelKind::Syn,
                        stream_id,
                        seqno: 0,
                        payload: Buff::copy_from_slice(
                            additional_data.clone().unwrap_or_default().as_bytes(),
                        ),
                    });
                })
                .detach();
            }
            Event::SendMsg(msg) => {
                trace_ctx.trace_pkt(&msg, true);
                let mut to_send = BuffMut::new();
                let r: &mut Vec<u8> = &mut to_send;
                bincode::serialize_into(r, &msg).unwrap();
                session.send_bytes(to_send.freeze()).await?;
            }
            Event::RecvMsg(msg) => {
                trace_ctx.trace_pkt(&msg, false);
                match msg {
                    // unreliable
                    Message::Urel(bts) => {
                        tracing::trace!("urel recv {}B", bts.len());
                    }
                    // connection opening
                    Message::Rel {
                        kind: RelKind::Syn,
                        stream_id,
                        payload,
                        ..
                    } => {
                        if conn_tab.get_stream(stream_id).is_some() {
                            tracing::trace!("syn recv {} REACCEPT", stream_id);
                            let msg = Message::Rel {
                                kind: RelKind::SynAck,
                                stream_id,
                                seqno: 0,
                                payload: Buff::copy_from_slice(&[]),
                            };
                            let mut bts = BuffMut::new();
                            bincode::serialize_into(bts.deref_mut(), &msg).unwrap();
                            session.send_bytes(bts.freeze()).await?;
                        } else {
                            tracing::trace!("syn recv {} ACCEPT", stream_id);
                            let lala = String::from_utf8_lossy(&payload).to_string();
                            let additional_info = if lala.is_empty() { None } else { Some(lala) };
                            let reap_dead = reap_dead.clone();
                            let (new_conn, new_conn_back) = RelConn::new(
                                RelConnState::SynReceived { stream_id },
                                glob_send.clone(),
                                move || {
                                    reap_dead(stream_id);
                                },
                                additional_info,
                            );
                            // the RelConn itself is responsible for sending the SynAck. Here we just store the connection into the table, accept it, and be done with it.
                            conn_tab.set_stream(stream_id, new_conn_back);
                            let _ = conn_accept_send.try_send(new_conn);
                        }
                    }
                    // associated with existing connection
                    Message::Rel {
                        stream_id, kind, ..
                    } => {
                        if let Some(handle) = conn_tab.get_stream(stream_id) {
                            // tracing::trace!("handing over {:?} to {}", kind, stream_id);
                            handle.process(msg)
                        } else {
                            tracing::trace!("discarding {:?} to nonexistent {}", kind, stream_id);
                            if kind != RelKind::Rst {
                                let msg = Message::Rel {
                                    kind: RelKind::Rst,
                                    stream_id,
                                    seqno: 0,
                                    payload: Buff::copy_from_slice(&[]),
                                };
                                let mut buf = BuffMut::new();
                                bincode::serialize_into(buf.deref_mut(), &msg).unwrap();
                                session.send_bytes(buf.freeze()).await?;
                            }
                        }
                    }
                    Message::Empty => {}
                }
            }
        }
    }
}

#[derive(Default)]
struct ConnTable {
    /// Maps IDs to RelConn back handles.
    sid_to_stream: DashMap<u16, RelConnBack>,
}

impl ConnTable {
    fn get_stream(&self, sid: u16) -> Option<RelConnBack> {
        let x = self.sid_to_stream.get(&sid)?;
        Some(x.clone())
    }

    fn set_stream(&self, id: u16, handle: RelConnBack) {
        self.sid_to_stream.insert(id, handle);
    }

    fn del_stream(&self, id: u16) {
        self.sid_to_stream.remove(&id);
    }

    fn find_id(&self) -> Option<u16> {
        if self.sid_to_stream.len() >= 65535 {
            tracing::warn!("ran out of descriptors ({})", self.sid_to_stream.len());
            return None;
        }
        loop {
            let possible_id: u16 = rand::thread_rng().gen();
            if self.sid_to_stream.get(&possible_id).is_none() {
                tracing::debug!(
                    "found id {} out of {}",
                    possible_id,
                    self.sid_to_stream.len()
                );
                break Some(possible_id);
            }
        }
    }
}
