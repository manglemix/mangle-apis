use std::{borrow::Cow, time::Duration};

use axum::extract::ws::{CloseFrame, Message, WebSocket};
use tokio::{
    select, spawn,
    sync::mpsc::{unbounded_channel, UnboundedReceiver, UnboundedSender},
    time::sleep,
};

const WEBSOCKET_PING: &str = "PING!!";

#[repr(u16)]
pub enum WebSocketCode {
    Ok = 1000,
    BadPayload = 1007,
    InternalError = 1011,
}

/// A wrapper around a WebSocket that does 3 things
/// 1. Pings the other end at a regular rate
/// 2. Provide a non-blocking way to send messages
/// 3. Provide an easy to use api with foolproof error handling
///
/// This is achieved by managing the WebSocket in a separate, asynchronous task
/// where messages are sent to and from
pub struct ManagedWebSocket {
    msg_sender: UnboundedSender<Message>,
    msg_receiver: UnboundedReceiver<Message>,
}

impl ManagedWebSocket {
    /// Wraps the given WebSocket and pings it every `ping_delay`.
    ///
    /// The timer for pinging is reset every time a message is sent or received
    pub fn new(mut ws: WebSocket, ping_delay: Duration) -> Self {
        let (msg_sender, mut task_msg_receiver) = unbounded_channel();
        let (task_msg_sender, msg_receiver) = unbounded_channel();

        spawn(async move {
            loop {
                select! {
                    // Ping Timer
                    () = sleep(ping_delay) => {
                        if ws.send(Message::Ping(WEBSOCKET_PING.as_bytes().to_vec()))
                            .await
                            .is_err()
                        {
                            return
                        }
                    }
                    // Message Receiver
                    opt = ws.recv() => {
                        if let Some(Ok(msg)) = opt {
                            if matches!(msg, Message::Pong(_)) || task_msg_sender.send(msg).is_ok() {
                                continue
                            }
                        }
                        return
                    }
                    // Message Sender
                    opt = task_msg_receiver.recv() => {
                        if let Some(msg) = opt {
                            let is_close = matches!(msg, Message::Close(_));
                            let res = ws.send(msg).await;

                            if is_close {
                                let _ = ws.close().await;
                                return
                            }

                            if res.is_err() {
                                return
                            }
                        } else {
                            let _ = ws.close().await;
                            return
                        }
                    }
                }
            }
        });

        Self {
            msg_sender,
            msg_receiver,
        }
    }

    /// Queues the given message for sending.
    ///
    /// Returns `None` if the WebSocket faced an error before this method was called,
    /// thus dropping `self`
    pub fn send(self, msg: impl Into<Message>) -> Option<Self> {
        if self.msg_sender.send(msg.into()).is_ok() {
            Some(self)
        } else {
            None
        }
    }

    /// Waits for a message from the other end of the WebSocket
    ///
    /// Returns `None` if the WebSocket faced an error before this method was called, or while waiting,
    /// thus dropping `self`
    pub async fn recv(mut self) -> Option<(Self, Message)> {
        self.msg_receiver.recv().await.map(move |msg| (self, msg))
    }

    /// Repeeatedly collects Messages from the other end of the WebSocket until an error occurs.
    ///
    /// If the WebSocket faced an error, the given `ws` Option will be set to None.
    ///
    /// If the given `msgs` is not None, the vector will be filled with Messages that were received.
    ///
    /// # Cancel Safety
    /// It is safe to cancel this Future; It will not stop the WebSocket from continuing to receive messages
    /// outside of the Future
    pub async fn loop_recv(ws: &mut Option<Self>, mut msgs: Option<&mut Vec<Message>>) {
        let Some(ws_mut) = ws.as_mut() else { return };
        loop {
            let Some(msg) = ws_mut.msg_receiver.recv().await else {
                *ws = None;
                return
            };
            if let Some(vec) = msgs.as_mut() {
                vec.push(msg);
            }
        }
    }

    pub fn close(self) {
        let _ = self.msg_sender.send(Message::Close(None));
    }

    pub fn close_frame(self, code: WebSocketCode, reason: impl Into<Cow<'static, str>>) {
        let _ = self.msg_sender.send(Message::Close(Some(CloseFrame {
            code: code as u16,
            reason: reason.into(),
        })));
    }
}
