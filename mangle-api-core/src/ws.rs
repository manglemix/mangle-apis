use std::{borrow::Cow, sync::Exclusive, time::Duration};

use axum::{
    async_trait,
    extract::ws::{CloseFrame, Message, WebSocket},
};
use messagist::text::TextStream;
use tokio::time::sleep;

const WEBSOCKET_PING: &str = "PING!!";

#[derive(derive_more::From, thiserror::Error, Debug)]
pub enum WsError {
    #[error("IOError {0}")]
    IOError(axum::Error),
    #[error("AlreadyClosed")]
    AlreadyClosed,
    #[error("NotAString")]
    NotAString(Vec<u8>),
}

#[repr(u16)]
pub enum WebSocketCode {
    Ok = 1000,
    BadPayload = 1007,
    InternalError = 1011,
}

pub struct ManagedWebSocket {
    ws: Exclusive<WebSocket>,
    ping_delay: Duration,
}

impl ManagedWebSocket {
    /// Wraps the given WebSocket and pings it every `ping_delay`.
    ///
    /// The timer for pinging is reset every time a message is sent or received
    pub fn new(ws: WebSocket, ping_delay: Duration) -> Self {
        Self {
            ws: Exclusive::new(ws),
            ping_delay,
        }
    }

    pub async fn close(
        &mut self,
        code: WebSocketCode,
        reason: impl Into<Cow<'static, str>>,
    ) -> Result<(), WsError> {
        self.ws
            .get_mut()
            .send(Message::Close(Some(CloseFrame {
                code: code as u16,
                reason: reason.into(),
            })))
            .await
            .map_err(Into::into)
    }
}

#[async_trait]
impl TextStream for ManagedWebSocket {
    type Error = WsError;
    async fn recv_string(&mut self) -> Result<String, Self::Error> {
        loop {
            let result;
            tokio::select! {
                () = sleep(self.ping_delay) => {
                    self.ws.get_mut().send(Message::Ping(WEBSOCKET_PING.as_bytes().to_vec())).await?;
                    continue
                }
                res = self.ws.get_mut().recv() => {
                    result = res;
                }
            }
            let Some(msg) = result else {
                break Err(WsError::AlreadyClosed)
            };
            match msg? {
                Message::Text(x) => break Ok(x),
                Message::Binary(x) => break Err(x.into()),
                Message::Ping(_) => unreachable!(),
                Message::Pong(_) => continue,
                Message::Close(_) => break Err(WsError::AlreadyClosed),
            }
        }
    }

    async fn send_string(&mut self, msg: String) -> Result<(), Self::Error> {
        self.ws
            .get_mut()
            .send(Message::Text(msg))
            .await
            .map_err(Into::into)
    }

    async fn wait_for_error(&mut self) -> Self::Error {
        loop {
            if let Err(e) = self.recv_string().await {
                break e;
            }
        }
    }
}
