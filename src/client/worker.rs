use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use anyhow::Context;
use smol::channel::{Receiver, Sender};

use crate::{backhaul::Backhaul, protocol::HandshakeFrame, runtime, Buff, SessionBack};

use super::inner::LowlevelClientConfig;

/// Encapsulates a worker "actor".
pub(crate) struct ClientWorker {
    received_count: Arc<AtomicUsize>,
    send_upload: Sender<Buff>,
    _task: smol::Task<()>,
}

impl ClientWorker {
    /// Spins off a new ClientWorker.
    pub fn start(
        cookie: crate::crypt::Cookie,
        resume_token: Buff,
        session_back: Arc<SessionBack>,
        shard_id: u8,
        cfg: LowlevelClientConfig,
    ) -> Self {
        let received_count = Arc::new(AtomicUsize::new(0));
        let (send_upload, recv_upload) = smol::channel::bounded(128);
        // spawn a task
        let _task = {
            let received_count = received_count.clone();
            runtime::spawn(async move {
                while let Err(err) = client_backhaul_once(
                    cookie.clone(),
                    resume_token.clone(),
                    session_back.clone(),
                    recv_upload.clone(),
                    shard_id,
                    cfg.clone(),
                    received_count.clone(),
                )
                .await
                {
                    tracing::error!("client_backhaul_once died: {:?}", err);
                    smol::Timer::after(Duration::from_secs(1)).await;
                }
            })
        };
        // create the stuff
        Self {
            received_count,
            send_upload,
            _task,
        }
    }

    /// Sends an upload through this ClientWorker.
    pub async fn send_upload(&self, buff: Buff) {
        self.send_upload.send(buff).await.expect("somehow died")
    }

    /// Reads the received count off of this ClientWorker
    pub fn get_received_count(&self) -> usize {
        self.received_count.load(Ordering::SeqCst)
    }

    /// Resets the received count.
    pub fn reset_received_count(&self) {
        self.received_count.store(0, Ordering::SeqCst)
    }
}

async fn client_backhaul_once(
    cookie: crate::crypt::Cookie,
    resume_token: Buff,
    session_back: Arc<SessionBack>,
    recv_upload: Receiver<Buff>,
    shard_id: u8,
    cfg: LowlevelClientConfig,
    received_count: Arc<AtomicUsize>,
) -> anyhow::Result<()> {
    let mut updated = false;
    let socket: Arc<dyn Backhaul> = (cfg.backhaul_gen)();
    // let mut _old_cleanup: Option<smol::Task<Option<()>>> = None;

    #[derive(Debug)]
    enum Evt {
        Incoming((Buff, SocketAddr)),
        Outgoing(Buff),
    }
    // last remind time
    let mut last_incoming_time: Option<Instant> = None;
    let mut last_outgoing_time: Option<Instant> = None;

    loop {
        let down = {
            let socket = &socket;
            async move {
                let packet = socket
                    .recv_from()
                    .await
                    .context("cannot receive from socket")?;
                Ok::<_, anyhow::Error>(Evt::Incoming(packet))
            }
        };
        let up = async {
            let raw_upload = recv_upload.recv().await?;
            Ok::<_, anyhow::Error>(Evt::Outgoing(raw_upload))
        };

        match smol::future::race(down, up).await {
            Ok(Evt::Incoming((bts, src))) => {
                tracing::trace!("received on shard {} from {}", shard_id, src);
                if src == cfg.server_addr {
                    received_count.fetch_add(1, Ordering::Relaxed);
                    let _ = session_back.inject_incoming(&bts);
                } else {
                    tracing::warn!("stray packet from {}", src)
                }
                last_incoming_time = Some(Instant::now());
            }
            Ok(Evt::Outgoing(bts)) => {
                let bts: Buff = bts;
                let now = Instant::now();
                if last_incoming_time
                    .map(|f| now.saturating_duration_since(f) > Duration::from_secs(1))
                    .unwrap_or_default()
                    || last_outgoing_time
                        .map(|f| now.saturating_duration_since(f) > Duration::from_secs(1))
                        .unwrap_or_default()
                    || !updated
                {
                    updated = true;
                    last_outgoing_time = Some(now);
                    let g_encrypt =
                        crate::crypt::LegacyAead::new(&cookie.generate_c2s().next().unwrap());
                    drop(
                        socket
                            .send_to(
                                g_encrypt.pad_encrypt_v1(
                                    &[HandshakeFrame::ClientResume {
                                        resume_token: resume_token.clone(),
                                        shard_id,
                                    }],
                                    1000,
                                ),
                                cfg.server_addr,
                            )
                            .await,
                    );
                }
                if let Err(err) = socket.send_to(bts, cfg.server_addr).await {
                    tracing::warn!("error sending packet: {:?}", err)
                }
            }
            Err(err) => {
                anyhow::bail!("FATAL error in down/up: {:?}", err);
            }
        }
    }
}
