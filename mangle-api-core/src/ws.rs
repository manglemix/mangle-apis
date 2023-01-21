use std::{sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};

use axum::extract::ws::{WebSocket, Message};
use tokio::{task::JoinHandle, spawn, time::sleep, sync::{Mutex, MutexGuard}};


const WEBSOCKET_POLL_DELAY: Duration = Duration::from_secs(45);
const WEBSOCKET_PING: &str = "PING!!";

pub struct PolledWebSocket {
    ws: Arc<Mutex<WebSocket>>,
    had_err: Arc<AtomicBool>,
    poller: JoinHandle<()>
}


impl PolledWebSocket {
    pub fn new(ws: WebSocket) -> Self {
        let ws = Arc::new(Mutex::new(ws));
        let ws2 = ws.clone();
        let had_err2 = Arc::new(AtomicBool::new(false));
        let had_err = had_err2.clone();

        let poller = spawn(async move {
            loop {
                sleep(WEBSOCKET_POLL_DELAY).await;

                if ws2
                    .lock()
                    .await
                    .send(Message::Ping(WEBSOCKET_PING.as_bytes().into()))
                    .await
                    .is_err() {
                        had_err2.store(true, Ordering::Release);
                        break
                    }
            }
        });

        PolledWebSocket { ws, had_err, poller }
    }

    pub async fn lock(&self) -> Option<MutexGuard<'_, WebSocket>> {
        if self.had_err.load(Ordering::Acquire) {
            None
        } else {
            Some(self.ws.lock().await)
        }
    }
}


impl Drop for PolledWebSocket {
    fn drop(&mut self) {
        self.poller.abort();
    }
}
