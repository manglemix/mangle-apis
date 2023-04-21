use std::{future::Future, io::Error, pin::Pin, task::Poll};

use crate::{bin::BinaryMessageStream, ExclusiveMessageHandler};
use async_trait::async_trait;
use interprocess::local_socket::tokio::{LocalSocketListener, LocalSocketStream};
pub use interprocess::local_socket::ToLocalSocketName;
use tokio::{spawn, task::JoinHandle};
use tokio_util::compat::{Compat, FuturesAsyncWriteCompatExt};

pub type LocalStream = Compat<LocalSocketStream>;

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

// pub struct DefaultListenerErrorHandler;

#[async_trait]
pub trait ListenerErrorHandler: Send + Sync + 'static {
    async fn handle_error(&self, err: Error);
}

// #[async_trait]
// impl<F, Fut> ListenerErrorHandler for F
// where
//     F: Fn(Error) -> Fut + Send + Sync + 'static,
//     Fut: Future<Output = ()> + Send
// {
//     async fn handle_error(&self, err: Error) {
//         (self)(err).await;
//     }
// }

// #[async_trait]
// impl ListenerErrorHandler for DefaultListenerErrorHandler {
//     async fn handle_error(&self, err: Error) {
//         error!("Faced the following error while listening for messages: {err}");
//     }
// }

pub fn start_listener<'a, H>(
    addr: impl ToLocalSocketName<'a>,
    mut handler: H,
) -> Result<ListenerHandle, Error>
where
    H: ExclusiveMessageHandler<SessionState = ()> + Send + ListenerErrorHandler + 'static,
{
    let listener = LocalSocketListener::bind(addr)?;

    Ok(ListenerHandle {
        handle: spawn(async move {
            loop {
                let stream = match listener.accept().await {
                    Ok(x) => x,
                    Err(e) => {
                        handler.handle_error(e).await;
                        continue;
                    }
                };

                handler
                    .handle(
                        BinaryMessageStream::from(FuturesAsyncWriteCompatExt::compat_write(stream)),
                        (),
                    )
                    .await;
            }
        }),
    })
}

pub async fn start_connection<'a>(
    addr: impl ToLocalSocketName<'a>,
) -> Result<BinaryMessageStream<LocalStream>, Error> {
    Ok(BinaryMessageStream::from(
        FuturesAsyncWriteCompatExt::compat_write(LocalSocketStream::connect(addr).await?),
    ))
}

// #[cfg(test)]
// mod tests {
//     use crate::{ExclusiveMessageHandler, MessageStream};

//     use super::*;

//     pub struct TestHandler {
//         sender: tokio::sync::mpsc::Sender<String>,
//     }
//     #[async_trait]
//     impl ExclusiveMessageHandler for TestHandler {
//         type SessionState = ();

//         async fn handle<S: MessageStream + Send>(
//             &mut self,
//             mut stream: S,
//             _session_state: Self::SessionState,
//         ) {
//             self.sender
//                 .send(stream.recv_message::<String>().await.unwrap())
//                 .await
//                 .unwrap();
//         }
//     }

//     #[tokio::test]
//     async fn test01() {
//         const MESSAGE: &str = "correffffffffffffffffffffffffffffffffffffffffffffffffwwwwwwctf";
//         let (sender, mut recv) = tokio::sync::mpsc::channel(1);
//         let _server =
//             start_listener("test", TestHandler { sender }, DefaultListenerErrorHandler).unwrap();
//         let mut stream = start_connection("test").await.unwrap();
//         stream.send_message(&MESSAGE.to_string()).await.unwrap();
//         assert!(recv.recv().await.unwrap() == MESSAGE);
//         stream.wait_for_error().await;
//     }
// }
