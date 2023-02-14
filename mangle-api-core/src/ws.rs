use std::{sync::{Arc}, time::Duration, mem::take, ops::{Deref, DerefMut}};

use axum::{extract::ws::{WebSocket, Message}, async_trait};
use tokio::{task::JoinHandle, spawn, time::sleep, sync::{Mutex, MutexGuard}};


const WEBSOCKET_POLL_DELAY: Duration = Duration::from_secs(45);
const WEBSOCKET_PING: &str = "PING!!";


pub struct BorrowedWebSocket<'a> {
    guard: MutexGuard<'a, Option<WebSocket>>,
    ws: Option<WebSocket>
}


impl<'a> Drop for BorrowedWebSocket<'a> {
    fn drop(&mut self) {
        *self.guard = take(&mut self.ws);
    }
}


impl<'a> Deref for BorrowedWebSocket<'a> {
    type Target = WebSocket;

    fn deref(&self) -> &Self::Target {
        self.ws.as_ref().unwrap()
    }
}


impl<'a> DerefMut for BorrowedWebSocket<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.ws.as_mut().unwrap()
    }
}


impl<'a> BorrowedWebSocket<'a> {
    pub fn into_inner(mut self) -> Option<WebSocket> {
        take(&mut self.ws)
    }
    pub fn take_inner(&mut self) -> Option<WebSocket> {
        take(&mut self.ws)
    }
    pub fn replace_inner(&mut self, ws: WebSocket) {
        self.ws = Some(ws);
    }
}


pub struct PolledWebSocket {
    ws: Arc<Mutex<Option<WebSocket>>>,
    poller: JoinHandle<()>
}


impl PolledWebSocket {
    pub fn new(ws: WebSocket) -> Self {
        let ws = Arc::new(Mutex::new(Some(ws)));
        let ws2 = ws.clone();

        let poller = spawn(async move {
            loop {
                sleep(WEBSOCKET_POLL_DELAY).await;

                let mut lock = ws2.lock().await;

                if let Some(ws) = lock.as_mut()
                {
                    if ws.send(Message::Ping(WEBSOCKET_PING.as_bytes().into()))
                        .await
                        .is_err()
                    {
                        let _ = take(lock.deref_mut())
                            .unwrap()
                            .close()
                            .await;
                    }
                }
            }
        });

        PolledWebSocket { ws, poller }
    }

    pub async fn lock(&self) -> Option<BorrowedWebSocket> {
        let mut guard = self.ws.lock().await;
        if guard.is_none() {
            None
        } else {
            let ws = take(guard.deref_mut());
            Some(BorrowedWebSocket { guard, ws })
        }
    }
}


impl Drop for PolledWebSocket {
    fn drop(&mut self) {
        self.poller.abort();
        take(self.ws.blocking_lock().deref_mut()).map(|x| x.close());
    }
}


#[async_trait]
pub trait WsExt: Sized {
    async fn easy_recv(self) -> Option<(Self, Message)>;
    async fn easy_send(self, msg: Message) -> Option<Self>;
    async fn safe_drop(self);
    async fn final_recv(self) -> Option<Message>;
    async fn final_send(self, msg: Message) -> bool;
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
        ws.safe_drop().await;
        Some(msg)
    }

    async fn final_send(self, msg: Message) -> bool {
        match self.easy_send(msg).await {
            Some(ws) => {
                ws.safe_drop().await;
                true
            }
            None => false
        }
    }

    async fn safe_drop(self) {
        let _ = self.close();
    }
}
