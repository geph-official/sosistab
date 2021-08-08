use parking_lot::Mutex;
use smol::future::Future;
use smol::prelude::*;
use std::{collections::VecDeque, pin::Pin, sync::Arc, task::Context, task::Poll};

use crate::buffer::Buff;

// /// Create a "bipe". Use async_dup's methods if you want something cloneable/shareable
// pub fn bipe(capacity: usize) -> (PipeWriter, PipeReader) {
//     let info = Arc::new(Mutex::new(BipeQueue::default()));
//     let event = Arc::new(event_listener::Event::new());
//     (
//         PipeWriter {
//             queue: info.clone(),
//             capacity,
//             signal: event.clone(),
//             listener: event.listen(),
//         },
//         PipeReader {
//             queue: info,
//             signal: event.clone(),
//             listener: event.listen(),
//         },
//     )
// }

// #[derive(Default)]
// struct BipeQueue {
//     inner: VecDeque<Buff>,
//     closed: bool,
//     counter: usize,
// }

// impl BipeQueue {
//     fn push(&mut self, bts: &[u8]) {
//         self.inner.push_front(Buff::copy_from_slice(bts));
//         self.counter += bts.len()
//     }

//     fn pop_fill(&mut self, fill: &mut [u8]) -> usize {
//         let tentative = self.inner.pop_back();
//         if let Some(tentative) = tentative {
//             assert!(self.counter >= tentative.len());
//             if tentative.len() <= fill.len() {
//                 fill[..tentative.len()].copy_from_slice(&tentative);
//                 self.counter -= tentative.len();
//                 tentative.len()
//             } else {
//                 fill.copy_from_slice(&tentative[..fill.len()]);
//                 let tentlen = tentative.len();
//                 let sliced = tentative.slice(fill.len()..);
//                 assert_eq!(sliced.len(), tentlen - fill.len());
//                 self.inner.push_back(sliced);
//                 self.counter -= fill.len();
//                 fill.len()
//             }
//         } else {
//             0
//         }
//     }
// }

// /// Writing end of a byte pipe.
// pub struct PipeWriter {
//     queue: Arc<Mutex<BipeQueue>>,
//     capacity: usize,
//     signal: Arc<event_listener::Event>,
//     listener: event_listener::EventListener,
// }

// impl Drop for PipeWriter {
//     fn drop(&mut self) {
//         self.queue.lock().closed = true;
//         self.signal.notify(usize::MAX);
//     }
// }

// fn broken_pipe() -> std::io::Error {
//     std::io::Error::new(std::io::ErrorKind::ConnectionReset, "broken pipe")
// }

// impl AsyncWrite for PipeWriter {
//     fn poll_write(
//         mut self: Pin<&mut Self>,
//         cx: &mut Context<'_>,
//         buf: &[u8],
//     ) -> Poll<std::io::Result<usize>> {
//         loop {
//             // if there's room in the buffer then it's fine
//             {
//                 let boo = &self.queue;
//                 let mut boo = boo.lock();
//                 if boo.closed {
//                     return Poll::Ready(Err(broken_pipe()));
//                 }
//                 if boo.counter < self.capacity + buf.len() {
//                     boo.push(buf);
//                     self.signal.notify(usize::MAX);
//                     return Poll::Ready(Ok(buf.len()));
//                 }
//             }
//             let listen_capacity = &mut self.listener;
//             smol::pin!(listen_capacity);
//             // there's no room, so we try again later
//             smol::ready!(listen_capacity.poll(cx));
//             self.listener = self.signal.listen()
//         }
//     }

//     fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
//         Poll::Ready(Ok(()))
//     }

//     fn poll_close(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
//         self.queue.lock().closed = true;
//         self.signal.notify(usize::MAX);
//         Poll::Ready(Ok(()))
//     }
// }

// /// Read end of a byte pipe.
// pub struct PipeReader {
//     queue: Arc<Mutex<BipeQueue>>,
//     signal: Arc<event_listener::Event>,
//     listener: event_listener::EventListener,
// }

// impl AsyncRead for PipeReader {
//     fn poll_read(
//         mut self: Pin<&mut Self>,
//         cx: &mut Context<'_>,
//         buf: &mut [u8],
//     ) -> Poll<std::io::Result<usize>> {
//         loop {
//             {
//                 let boo = &self.queue;
//                 let mut boo = boo.lock();
//                 if boo.counter > 0 {
//                     let to_copy_len = boo.pop_fill(buf);
//                     self.signal.notify(usize::MAX);
//                     return Poll::Ready(Ok(to_copy_len));
//                 }
//                 if boo.closed {
//                     return Poll::Ready(Err(broken_pipe()));
//                 }
//             }
//             let listen_new_data = &mut self.listener;
//             smol::pin!(listen_new_data);
//             smol::ready!(listen_new_data.poll(cx));
//             self.listener = self.signal.listen();
//         }
//     }
// }
