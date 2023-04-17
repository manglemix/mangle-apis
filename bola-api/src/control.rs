use std::{future::Future, pin::Pin};

use axum::async_trait;
use messagist::{ExclusiveMessageHandler, MessageStream};
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
    type Output = ();

    fn poll(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Self::Output> {
        Pin::new(&mut self.stop_recv).poll_recv(cx).map(|_| ())
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
    type Error = anyhow::Error;

    async fn handle<S: MessageStream + Send>(&mut self, mut stream: S) -> Result<(), Self::Error> {
        let msg: ControlClientMessage = stream.recv_message().await?;
        match msg {
            ControlClientMessage::Stop => {
                let _ = self.stop_sender.send(()).await;
                Ok(())
            }
        }
    }
}
