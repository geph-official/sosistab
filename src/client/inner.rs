use crate::{buffer::Buff, crypt};
use crate::{protocol, runtime, Backhaul, Session, SessionConfig, StatsGatherer};

use probability::distribution::{Binomial, Distribution};
use smallvec::SmallVec;
use smol::{prelude::*, Task};
use std::{
    collections::VecDeque,
    net::SocketAddr,
    sync::Arc,
    time::{Duration, Instant},
};

use super::worker::ClientWorker;

/// Configures the client.
#[derive(Clone)]
pub(crate) struct LowlevelClientConfig {
    pub server_addr: SocketAddr,
    pub server_pubkey: x25519_dalek::PublicKey,
    pub backhaul_gen: Arc<dyn Fn() -> Arc<dyn Backhaul> + 'static + Send + Sync>,
    pub num_shards: usize,
    pub reset_interval: Option<Duration>,
    pub gather: Arc<StatsGatherer>,
}

/// Connects to a remote server, given a closure that generates socket addresses.
pub(crate) async fn connect_custom(cfg: LowlevelClientConfig) -> std::io::Result<Session> {
    let my_long_sk = x25519_dalek::StaticSecret::new(&mut rand::thread_rng());
    let my_eph_sk = x25519_dalek::StaticSecret::new(&mut rand::thread_rng());
    // do the handshake
    let cookie = crypt::Cookie::new(cfg.server_pubkey);
    let init_hello = protocol::HandshakeFrame::ClientHello {
        long_pk: (&my_long_sk).into(),
        eph_pk: (&my_eph_sk).into(),
        version: VERSION,
    };
    for timeout_factor in (0u32..).map(|x| 2u64.pow(x.min(10))) {
        let backhaul = (cfg.backhaul_gen)();
        // send hello
        let init_hello = crypt::LegacyAead::new(&cookie.generate_c2s().next().unwrap())
            .pad_encrypt_v1(std::slice::from_ref(&init_hello), 1000);
        backhaul.send_to(init_hello, cfg.server_addr).await?;
        tracing::trace!("sent client hello");
        // wait for response
        let res = backhaul
            .recv_from()
            .or(async {
                smol::Timer::after(Duration::from_secs(timeout_factor.min(10))).await;
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out",
                ))
            })
            .await;
        match res {
            Ok((buf, _)) => {
                for possible_key in cookie.generate_s2c() {
                    let decrypter = crypt::LegacyAead::new(&possible_key);
                    let response = decrypter.pad_decrypt_v1(&buf);
                    for response in response.unwrap_or_default() {
                        if let protocol::HandshakeFrame::ServerHello {
                            long_pk,
                            eph_pk,
                            resume_token,
                        } = response
                        {
                            tracing::trace!("obtained response from server");
                            if long_pk.as_bytes() != cfg.server_pubkey.as_bytes() {
                                return Err(std::io::Error::new(
                                    std::io::ErrorKind::ConnectionRefused,
                                    "bad pubkey",
                                ));
                            }
                            let shared_sec =
                                crypt::triple_ecdh(&my_long_sk, &my_eph_sk, &long_pk, &eph_pk);
                            return Ok(init_session(cookie, resume_token, shared_sec, cfg.clone()));
                        }
                    }
                }
            }
            Err(err) => {
                if err.kind() == std::io::ErrorKind::TimedOut {
                    tracing::trace!(
                        "timed out to {} with {}s timeout; trying again",
                        cfg.server_addr,
                        timeout_factor
                    );
                    continue;
                }
                return Err(err);
            }
        }
    }
    unimplemented!()
}
const VERSION: u64 = 3;

fn init_session(
    cookie: crypt::Cookie,
    resume_token: Buff,
    shared_sec: blake3::Hash,
    cfg: LowlevelClientConfig,
) -> Session {
    let (mut session, back) = Session::new(SessionConfig {
        version: VERSION,
        gather: cfg.gather.clone(),
        session_key: shared_sec.as_bytes().to_vec(),
        role: crate::Role::Client,
    });
    let back = Arc::new(back);
    let uploader: Task<anyhow::Result<()>> = runtime::spawn(async move {
        let mut workers: Vec<ClientWorker> = (0..cfg.num_shards)
            .map(|shard_id| {
                ClientWorker::start(
                    cookie.clone(),
                    resume_token.clone(),
                    back.clone(),
                    shard_id as u8,
                    cfg.clone(),
                )
            })
            .collect();
        let mut fired_workers: VecDeque<ClientWorker> = VecDeque::new();
        let mut last_reset = Instant::now();
        let mut just_respawned = false;
        for ctr in (0..).cycle() {
            let to_upload = back.next_outgoing().await?;
            let random_worker = ctr % workers.len();
            workers[random_worker].send_upload(to_upload).await;
            if cfg
                .reset_interval
                .map(|dur| last_reset.elapsed() > dur)
                .unwrap_or_default()
            {
                tracing::debug!("reset timer expired!");
                last_reset = Instant::now();
                if just_respawned {
                    for worker in workers.iter() {
                        worker.reset_received_count();
                    }
                    just_respawned = false;
                } else {
                    // check: are we even that bad?
                    let worker_packet_count: SmallVec<[usize; 16]> =
                        workers.iter().map(|w| w.get_received_count()).collect();
                    let p_value = uniform_pvalue(&worker_packet_count);
                    tracing::debug!("p-value = {}; {:?}", p_value, worker_packet_count);
                    if p_value < 0.01 {
                        // find the worst worker and fire it
                        let worst_worker_id = workers
                            .iter()
                            .enumerate()
                            .min_by_key(|(worker_id, worker)| {
                                let count = worker.get_received_count();
                                tracing::debug!("worker {} has {}", worker_id, count);
                                count
                            })
                            .map(|x| x.0)
                            .expect("must have a worst worker");
                        tracing::debug!("replacing worst worker {}", worst_worker_id);
                        let new_worker = ClientWorker::start(
                            cookie.clone(),
                            resume_token.clone(),
                            back.clone(),
                            worst_worker_id as u8,
                            cfg.clone(),
                        );
                        let worst_worker =
                            std::mem::replace(&mut workers[worst_worker_id], new_worker);
                        fired_workers.push_back(worst_worker);
                        if fired_workers.len() > workers.len() {
                            fired_workers.pop_front();
                        }
                        just_respawned = true;
                    }
                }
            }
        }
        unreachable!()
    });
    session.on_drop(move || {
        drop(uploader);
    });
    session
}

// guess whether the given slice is uniformly distributed
fn uniform_pvalue(vals: &[usize]) -> f64 {
    if vals.is_empty() {
        return 0.0;
    }
    let total_count = vals.iter().sum::<usize>();
    let min = vals.iter().min().copied().unwrap();
    let distro = Binomial::new(total_count, 1.0 / (vals.len() as f64));
    distro.distribution(min as f64)
}
