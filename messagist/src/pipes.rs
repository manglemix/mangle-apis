use std::{future::Future, pin::Pin, task::Poll};

use crate::{bin::BinaryMessageStream, HandlerErrorInner, MessageHandler};
use futures_io::{AsyncRead, AsyncWrite};
use interprocess::local_socket::tokio::{LocalSocketListener, LocalSocketStream};
pub use interprocess::local_socket::ToLocalSocketName;
use tokio::{spawn, task::JoinHandle};

pub struct LocalStream(LocalSocketStream);

impl tokio::io::AsyncRead for LocalStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.0)
            .poll_read(cx, buf.initialized_mut())
            .map(|x| x.map(|_| ()))
    }
}

impl tokio::io::AsyncWrite for LocalStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.0).poll_write(cx, buf)
    }

    fn poll_flush(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_flush(cx)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.0).poll_close(cx)
    }
}

pub enum ListenerError<H>
where
    H: MessageHandler,
{
    AcceptError(futures_io::Error),
    HandlerError(HandlerErrorInner<H>),
}

impl<H: MessageHandler> std::fmt::Debug for ListenerError<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AcceptError(arg0) => f.debug_tuple("AcceptError").field(arg0).finish(),
            Self::HandlerError(arg0) => f.debug_tuple("HandlerError").field(arg0).finish(),
        }
    }
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

pub fn start_listener<'a, H, F, Fut>(
    addr: impl ToLocalSocketName<'a>,
    mut handler: H,
    error_handler: F,
) -> Result<ListenerHandle, std::io::Error>
where
    H: MessageHandler<
            RecoverableError: Send + Sync + 'static,
            IrrecoverableError: Send + Sync + 'static,
        > + Send
        + 'static,
    F: Fn(ListenerError<H>) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = ()> + Send,
{
    let listener = LocalSocketListener::bind(addr)?;

    Ok(ListenerHandle {
        handle: spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        (error_handler)(ListenerError::AcceptError(e)).await;
                        continue;
                    }
                };

                match handler
                    .handle(BinaryMessageStream::from(LocalStream(stream)))
                    .await
                {
                    Ok(x) => handler = x,
                    Err(e) => {
                        let (x, e) = e.into_inner();

                        (error_handler)(ListenerError::HandlerError(e)).await;

                        let Some(x) = x else {
                            break
                        };
                        handler = x;
                    }
                }
            }
        }),
    })
}

pub async fn start_connection<'a>(
    addr: impl ToLocalSocketName<'a>,
) -> Result<BinaryMessageStream<LocalStream>, std::io::Error> {
    Ok(BinaryMessageStream::from(LocalStream(
        LocalSocketStream::connect(addr).await?,
    )))
}
