//! Native tools and HTTP/source tasks enter the single SQL owner here. Requests
//! contain trusted Rust closures, never model-supplied code or another history.
#![forbid(unsafe_code)]
#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::let_underscore_must_use,
    unused_must_use
)]
#![cfg_attr(
    not(test),
    forbid(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::let_underscore_must_use,
        unused_must_use
    )
)]
use super::Application;
use crate::{Error, Result};
use futures::{future::LocalBoxFuture, stream::FuturesUnordered, FutureExt, StreamExt};
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

type Work = Box<dyn FnOnce(&Application) -> LocalBoxFuture<'static, ()> + Send>;
pub(super) struct Request {
    work: Work,
}
#[derive(Clone)]
pub struct Client {
    sender: mpsc::Sender<Request>,
    abandoned: Arc<AtomicU64>,
}
fn closed() -> Error {
    Error::Message("SuperAgent application owner is unavailable".into())
}
pub(super) fn channel() -> (Client, mpsc::Receiver<Request>) {
    let (sender, receiver) = mpsc::channel(64);
    (
        Client {
            sender,
            abandoned: Arc::new(AtomicU64::new(0)),
        },
        receiver,
    )
}
impl Client {
    pub async fn call<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Application) -> Result<T> + Send + 'static,
    {
        self.call_async(move |app| futures::future::ready(work(app)).boxed_local())
            .await
    }
    pub async fn call_async<T, F>(&self, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Application) -> LocalBoxFuture<'static, Result<T>> + Send + 'static,
    {
        self.call_with_cancel(CancellationToken::new(), work).await
    }
    pub async fn call_with_cancel<T, F>(&self, cancel: CancellationToken, work: F) -> Result<T>
    where
        T: Send + 'static,
        F: FnOnce(&Application) -> LocalBoxFuture<'static, Result<T>> + Send + 'static,
    {
        let (send, receive) = oneshot::channel();
        let abandoned = self.abandoned.clone();
        let before = cancel.clone();
        let request = Request {
            work: Box::new(move |app| {
                let result = if before.is_cancelled() {
                    futures::future::ready(Err(Error::Message(
                        "Application request cancelled before execution".into(),
                    )))
                    .boxed_local()
                } else {
                    work(app)
                };
                async move {
                    // Once admitted, the owner completes the work even if the caller
                    // goes away. A lost response is visible, not a rolled-back commit.
                    if send.send(result.await).is_err() {
                        abandoned.fetch_add(1, Ordering::Relaxed);
                    }
                }
                .boxed_local()
            }),
        };
        tokio::select! {
            biased;
            _ = cancel.cancelled() => return Err(Error::Message("Application request cancelled before admission".into())),
            result = self.sender.send(request) => result.map_err(|_| closed())?,
        }
        receive.await.map_err(|_| closed())?
    }
    pub fn abandoned_responses(&self) -> u64 {
        self.abandoned.load(Ordering::Relaxed)
    }
}
pub(super) async fn run(app: Application, mut receiver: mpsc::Receiver<Request>) {
    let mut work = FuturesUnordered::new();
    loop {
        tokio::select! {
            Some(request) = receiver.recv() => work.push((request.work)(&app)),
            Some(()) = work.next(), if !work.is_empty() => (),
            else => return,
        }
    }
}
