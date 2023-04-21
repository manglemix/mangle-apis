use std::{future::Future, pin::Pin, task::Poll};

use axum::async_trait;
use log::error;
use messagist::{pipes::ListenerErrorHandler, ExclusiveMessageHandler, MessageStream};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
pub enum ControlServerMessage {}

#[derive(Serialize, Deserialize)]
pub enum ControlClientMessage {
    Stop,
}

pub struct ControlHandlerReceiver {
    stop_recv: tokio::sync::mpsc::Receiver<()>,
}

impl Future for ControlHandlerReceiver {
    type Output = String;

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> Poll<Self::Output> {
        if Pin::new(&mut self.stop_recv).poll_recv(cx).is_ready() {
            Poll::Ready("Stop command issued".into())
        } else {
            Poll::Pending
        }
    }
}

pub struct ControlHandler {
    stop_sender: tokio::sync::mpsc::Sender<()>,
}

pub fn new_control_handler() -> (ControlHandler, ControlHandlerReceiver) {
    let (stop_sender, stop_recv) = tokio::sync::mpsc::channel(1);
    (
        ControlHandler { stop_sender },
        ControlHandlerReceiver { stop_recv },
    )
}

#[async_trait]
impl ExclusiveMessageHandler for ControlHandler {
    type SessionState = ();

    async fn handle<S: MessageStream + Send>(
        &mut self,
        mut stream: S,
        _session_state: Self::SessionState,
    ) {
        let msg: ControlClientMessage = match stream.recv_message().await {
            Ok(x) => x,
            Err(e) => {
                error!("Error receiving message: {e}");
                return;
            }
        };
        match msg {
            ControlClientMessage::Stop => {
                let _ = self.stop_sender.send(()).await;
            }
        }
    }
}

#[async_trait]
impl ListenerErrorHandler for ControlHandler {
    async fn handle_error(&self, err: std::io::Error) {
        error!("Error accepting stream: {err}")
    }
}
