use std::{future::Future, pin::Pin, task::Poll};

use crate::{bin::BinaryMessageStream, ExclusiveMessageHandler};
use async_trait::async_trait;
use interprocess::local_socket::tokio::{LocalSocketListener, LocalSocketStream};
pub use interprocess::local_socket::ToLocalSocketName;
use log::error;
use tokio::{spawn, task::JoinHandle};
use tokio_util::compat::{Compat, FuturesAsyncWriteCompatExt};

// pub struct LocalStream(LocalSocketStream);

// impl tokio::io::AsyncRead for LocalStream {
//     fn poll_read(
//         mut self: Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//         buf: &mut tokio::io::ReadBuf<'_>,
//     ) -> Poll<std::io::Result<()>> {
//         Pin::new(&mut self.0)
//             .poll_read(cx, buf.initialized_mut())
//             .map(|x| x.map(|_| ()))
//     }
// }

// impl tokio::io::AsyncWrite for LocalStream {
//     fn poll_write(
//         mut self: Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//         buf: &[u8],
//     ) -> Poll<Result<usize, std::io::Error>> {
//         Pin::new(&mut self.0).poll_write(cx, buf)
//     }

//     fn poll_flush(
//         mut self: Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//     ) -> Poll<Result<(), std::io::Error>> {
//         Pin::new(&mut self.0).poll_flush(cx)
//     }

//     fn poll_shutdown(
//         mut self: Pin<&mut Self>,
//         cx: &mut std::task::Context<'_>,
//     ) -> Poll<Result<(), std::io::Error>> {
//         Pin::new(&mut self.0).poll_close(cx)
//     }
// }

pub type LocalStream = Compat<LocalSocketStream>;

#[derive(Debug)]
pub enum ListenerError<E> {
    AcceptError(futures_io::Error),
    HandlerError(E),
}

pub struct ListenerHandle {
    handle: JoinHandle<()>,
}

impl Drop for ListenerHandle {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

impl ListenerHandle {
    pub fn detach(self) {
        std::mem::forget(self);
    }
}

impl Future for ListenerHandle {
    type Output = Result<(), tokio::task::JoinError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.handle).poll(cx)
    }
}

pub struct DefaultListenerErrorHandler;

#[async_trait]
pub trait ListenerErrorHandler<H: ExclusiveMessageHandler>: Send + Sync + 'static {
    async fn handle_error(&self, err: ListenerError<H::Error>);
}

#[async_trait]
impl<F, Fut, H> ListenerErrorHandler<H> for F
where
    H: ExclusiveMessageHandler<Error: Send> + 'static,
    F: Fn(ListenerError<H::Error>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    async fn handle_error(&self, err: ListenerError<H::Error>) {
        (self)(err).await;
    }
}

#[async_trait]
impl<H> ListenerErrorHandler<H> for DefaultListenerErrorHandler
where
    H: ExclusiveMessageHandler<Error: std::fmt::Debug + Send> + 'static,
{
    async fn handle_error(&self, err: ListenerError<H::Error>) {
        error!("Faced the following error while listening for messages: {err:?}");
    }
}

pub fn start_listener<'a, H, F>(
    addr: impl ToLocalSocketName<'a>,
    mut handler: H,
    error_handler: F,
) -> Result<ListenerHandle, std::io::Error>
where
    H: ExclusiveMessageHandler<Error: Send> + Send + 'static,
    F: ListenerErrorHandler<H>,
{
    let listener = LocalSocketListener::bind(addr)?;

    Ok(ListenerHandle {
        handle: spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        error_handler
                            .handle_error(ListenerError::AcceptError(e))
                            .await;
                        continue;
                    }
                };

                if let Err(e) = handler
                    .handle(BinaryMessageStream::from(
                        FuturesAsyncWriteCompatExt::compat_write(stream),
                    ))
                    .await
                {
                    error_handler
                        .handle_error(ListenerError::HandlerError(e))
                        .await;
                }
            }
        }),
    })
}

pub async fn start_connection<'a>(
    addr: impl ToLocalSocketName<'a>,
) -> Result<BinaryMessageStream<LocalStream>, std::io::Error> {
    Ok(BinaryMessageStream::from(
        FuturesAsyncWriteCompatExt::compat_write(LocalSocketStream::connect(addr).await?),
    ))
}

#[cfg(test)]
mod tests {
    use crate::{ExclusiveMessageHandler, MessageStream};

    use super::*;

    pub struct TestHandler {
        sender: tokio::sync::mpsc::Sender<String>,
    }
    #[async_trait]
    impl ExclusiveMessageHandler for TestHandler {
        type Error = ();

        async fn handle<S: MessageStream + Send>(
            &mut self,
            mut stream: S,
        ) -> Result<(), Self::Error> {
            self.sender
                .send(stream.recv_message::<String>().await.unwrap())
                .await
                .unwrap();
            Ok(())
        }
    }

    #[tokio::test]
    async fn test01() {
        const MESSAGE: &str = "correffffffffffffffffffffffffffffffffffffffffffffffffwwwwwwctf";
        let (sender, mut recv) = tokio::sync::mpsc::channel(1);
        let _server =
            start_listener("test", TestHandler { sender }, DefaultListenerErrorHandler).unwrap();
        let mut stream = start_connection("test").await.unwrap();
        stream.send_message(&MESSAGE.to_string()).await.unwrap();
        assert!(recv.recv().await.unwrap() == MESSAGE);
        stream.wait_for_error().await;
    }
}
