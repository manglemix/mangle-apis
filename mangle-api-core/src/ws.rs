use std::{
    borrow::Cow,
    mem::take,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

use axum::{
    async_trait,
    extract::ws::{CloseFrame, Message, WebSocket},
};
use tokio::{
    spawn,
    sync::{
        mpsc::{unbounded_channel, UnboundedReceiver},
        Mutex, OwnedMutexGuard,
    },
    task::JoinHandle,
    time::sleep,
};

const WEBSOCKET_POLL_DELAY: Duration = Duration::from_secs(45);
const WEBSOCKET_PING: &str = "PING!!";

struct EmptyBorrowedWebSocketImpl {
    guard: OwnedMutexGuard<Option<WebSocket>>,
    poller: JoinHandle<()>,
    failed_receiver: UnboundedReceiver<()>,
}

/// A lock of a PolledWebSocket that does not contain a WebSocket
///
/// The Websocket can be replaced to allows the polling thread to continue
/// when the lock is dropped
///
/// The WebSocket is not polled for as long as this struct is alive
pub struct EmptyBorrowedWebSocket {
    inner: Option<EmptyBorrowedWebSocketImpl>,
}

struct BorrowedWebSocketImpl {
    guard: OwnedMutexGuard<Option<WebSocket>>,
    ws: WebSocket,
    poller: JoinHandle<()>,
    failed_receiver: UnboundedReceiver<()>,
}

/// A lock of a PolledWebSocket where ownership of the WebSocket is guaranteed
///
/// As a result, this struct dereferences to a WebSocket
///
/// The WebSocket is not polled for as long as this struct is alive
pub struct BorrowedWebSocket {
    // Always Some except in Drop
    inner: Option<BorrowedWebSocketImpl>,
}

struct PolledWebSocketImpl {
    lock: Arc<Mutex<Option<WebSocket>>>,
    poller: JoinHandle<()>,
    failed_receiver: UnboundedReceiver<()>,
}

/// A WebSocket that is polled when not locked (ie. when this struct is alive)
///
/// Polling of a WebSocket allows the connection to remain alive indefinitely,
/// so long as the other end responds to the polls
pub struct PolledWebSocket {
    inner: Option<PolledWebSocketImpl>,
}

impl EmptyBorrowedWebSocket {
    /// Replace the WebSocket so that it can be polled when the lock is dropped
    pub fn replace_ws(mut self, ws: WebSocket) -> BorrowedWebSocket {
        let inner = take(&mut self.inner).unwrap();
        BorrowedWebSocket {
            inner: Some(BorrowedWebSocketImpl {
                guard: inner.guard,
                ws,
                poller: inner.poller,
                failed_receiver: inner.failed_receiver,
            }),
        }
    }
}

impl Drop for EmptyBorrowedWebSocket {
    fn drop(&mut self) {
        take(&mut self.inner).unwrap().poller.abort();
    }
}

impl BorrowedWebSocket {
    /// Stops the polling thread permanently (it is paused for as long as this struct is alive)
    /// and returns the inner WebSocket
    pub fn into_inner(mut self) -> WebSocket {
        take(&mut self.inner).unwrap().ws
    }
    /// Returns the inner WebSocket while keeping the polling thread paused
    pub fn borrow_inner(mut self) -> (WebSocket, EmptyBorrowedWebSocket) {
        let inner = take(&mut self.inner).unwrap();
        (
            inner.ws,
            EmptyBorrowedWebSocket {
                inner: Some(EmptyBorrowedWebSocketImpl {
                    guard: inner.guard,
                    poller: inner.poller,
                    failed_receiver: inner.failed_receiver,
                }),
            },
        )
    }
    /// Unpauses the polling thread
    pub fn unlock(mut self) -> PolledWebSocket {
        let mut inner = take(&mut self.inner).unwrap();
        *inner.guard = Some(inner.ws);
        PolledWebSocket {
            inner: Some(PolledWebSocketImpl {
                lock: OwnedMutexGuard::mutex(&inner.guard).clone(),
                poller: inner.poller,
                failed_receiver: inner.failed_receiver,
            }),
        }
    }
}

impl Drop for BorrowedWebSocket {
    fn drop(&mut self) {
        let inner = take(&mut self.inner).unwrap();
        inner.poller.abort();
        spawn(inner.ws.close());
    }
}

impl Deref for BorrowedWebSocket {
    type Target = WebSocket;

    fn deref(&self) -> &Self::Target {
        &self.inner.as_ref().unwrap().ws
    }
}

impl DerefMut for BorrowedWebSocket {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.inner.as_mut().unwrap().ws
    }
}

impl PolledWebSocket {
    /// Wrap the given WebSocket, allowing it to be polled while not locked
    pub fn new(ws: WebSocket) -> Self {
        let lock = Arc::new(Mutex::new(Some(ws)));
        let lock2 = lock.clone();
        let (sender, failed_receiver) = unbounded_channel();

        let poller = spawn(async move {
            let _sender = sender;
            loop {
                sleep(WEBSOCKET_POLL_DELAY).await;

                let mut guard = lock2.lock().await;

                if let Some(ws) = guard.as_mut() {
                    if ws
                        .send(Message::Ping(WEBSOCKET_PING.as_bytes().into()))
                        .await
                        .is_err()
                    {
                        take(guard.deref_mut()).unwrap().close_ignored().await;
                    }
                } else {
                    break;
                }
            }
        });

        PolledWebSocket {
            inner: Some(PolledWebSocketImpl {
                lock,
                poller,
                failed_receiver,
            }),
        }
    }

    /// Locks the inner WebSocket, allowing it to be used
    ///
    /// If None is returned, the WebSocket faced an error the last time it was polled,
    /// rendering it unusable
    pub async fn lock(mut self) -> Option<BorrowedWebSocket> {
        let inner = take(&mut self.inner).unwrap();
        let mut guard = inner.lock.lock_owned().await;

        if let Some(ws) = take(guard.deref_mut()) {
            Some(BorrowedWebSocket {
                inner: Some(BorrowedWebSocketImpl {
                    guard,
                    ws: ws,
                    poller: inner.poller,
                    failed_receiver: inner.failed_receiver,
                }),
            })
        } else {
            None
        }
    }

    /// Waits until the polling thread faced an error while using the inner WebSocket
    pub async fn wait_for_failure(&mut self) {
        self.inner.as_mut().unwrap().failed_receiver.recv().await;
    }

    /// Stops the polling thread and returns the inner WebSocket
    ///
    /// If None is returned, the WebSocket faced an error the last time it was polled,
    /// rendering it unusable
    pub async fn into_inner(mut self) -> Option<WebSocket> {
        let inner = take(&mut self.inner).unwrap();
        inner.poller.abort();
        let mut guard = inner.lock.lock().await;
        take(&mut guard.deref_mut())
    }
}

impl Drop for PolledWebSocket {
    fn drop(&mut self) {
        if let Some(inner) = take(&mut self.inner) {
            inner.poller.abort();
            let mut lock = inner.lock.blocking_lock();
            if let Some(ws) = take(lock.deref_mut()) {
                spawn(ws.close());
            }
        }
    }
}

#[async_trait]
pub trait WsExt: Sized {
    /// Waits for a message, or safely drops when an error is faced
    async fn easy_recv(self) -> Option<(Self, Message)>;
    /// Sends a message, or safely drops when an error is faced
    async fn easy_send(self, msg: Message) -> Option<Self>;
    /// Closes while ignoring the result
    async fn close_ignored(self);
    /// Receive one last message and safely drop regardless of what happened
    async fn final_recv(self) -> Option<Message>;
    /// Send one last message and safely drop regardless of what happened
    /// 
    /// Returns true iff the message could not be sent
    async fn final_send(self, msg: Message) -> bool;
    /// Send a close frame and safely drop regardless of what happened
    /// 
    /// Returns true iff the message could not be sent
    async fn final_send_close_frame(
        self,
        code: u16,
        reason: impl Into<Cow<'static, str>> + Send + Sync,
    ) -> bool;
    async fn close_internal_error(self) -> bool {
        self.final_send_close_frame(1011, "Internal Error").await
    }
    async fn close_bad_payload(self) -> bool {
        self.final_send_close_frame(1007, "Bad Payload").await
    }
    async fn close_success(self) -> bool {
        self.final_send_close_frame(1000, "").await
    }

}

#[async_trait]
impl WsExt for WebSocket {
    async fn easy_recv(mut self) -> Option<(Self, Message)> {
        match self.recv().await? {
            Ok(msg) => Some((self, msg)),
            Err(_) => {
                let _ = self.close();
                None
            }
        }
    }

    async fn easy_send(mut self, msg: Message) -> Option<Self> {
        match self.send(msg).await {
            Ok(()) => Some(self),
            Err(_) => {
                let _ = self.close();
                None
            }
        }
    }

    async fn final_recv(self) -> Option<Message> {
        let (ws, msg) = self.easy_recv().await?;
        ws.close_ignored().await;
        Some(msg)
    }

    async fn final_send(self, msg: Message) -> bool {
        match self.easy_send(msg).await {
            Some(ws) => {
                ws.close_ignored().await;
                true
            }
            None => false,
        }
    }

    async fn final_send_close_frame(
        self,
        code: u16,
        reason: impl Into<Cow<'static, str>> + Send + Sync,
    ) -> bool {
        self.final_send(Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })))
        .await
    }

    async fn close_ignored(self) {
        let _ = self.close();
    }
}
