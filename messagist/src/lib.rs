#![feature(associated_type_defaults)]
#![feature(never_type)]
#![feature(associated_type_bounds)]
#![feature(box_into_inner)]

use std::error::Error;
use std::fmt::Debug;

use async_trait::async_trait;
use serde::Deserialize;
use serde::{de::DeserializeOwned, Serialize};

#[cfg(feature = "bincode")]
pub mod bin;
#[cfg(feature = "pipes")]
pub mod pipes;
#[cfg(feature = "json")]
pub mod text;

pub enum RecvError<S: MessageStream> {
    IOError(std::io::Error),
    DeserializeError(S::DeserializeError, S),
}

#[async_trait]
pub trait MessageStream: Sized {
    type DeserializeError;

    async fn recv_message<T>(self) -> Result<(T, Self), RecvError<Self>>
    where
        T: DeserializeOwned + Send;

    async fn recv_borrowed_message<T, F, O>(self, f: F) -> Result<(O, Self), RecvError<Self>>
    where
        for<'a> T: Deserialize<'a> + Send,
        F: FnOnce(T) -> O + Send;

    async fn send_message<T: Serialize + Sync>(self, msg: &T) -> Result<Self, std::io::Error>;
}

pub enum HandlerErrorInner<H: MessageHandler> {
    Recoverable(H::RecoverableError),
    Irrecoverable(H::IrrecoverableError),
}

impl<H: MessageHandler> Debug for HandlerErrorInner<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recoverable(arg0) => f.debug_tuple("Recoverable").field(arg0).finish(),
            Self::Irrecoverable(arg0) => f.debug_tuple("Irrecoverable").field(arg0).finish(),
        }
    }
}

pub enum HandlerError<H: MessageHandler> {
    Recoverable(H, H::RecoverableError),
    Irrecoverable(H::IrrecoverableError),
}

impl<H: MessageHandler> Debug for HandlerError<H> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Recoverable(_, arg0) => f.debug_tuple("Recoverable").field(arg0).finish(),
            Self::Irrecoverable(arg0) => f.debug_tuple("Irrecoverable").field(arg0).finish(),
        }
    }
}

impl<S: MessageHandler> HandlerError<S> {
    pub fn into_inner(self) -> (Option<S>, HandlerErrorInner<S>) {
        match self {
            HandlerError::Recoverable(s, e) => (Some(s), HandlerErrorInner::Recoverable(e)),
            HandlerError::Irrecoverable(e) => (None, HandlerErrorInner::Irrecoverable(e)),
        }
    }
}

#[async_trait]
pub trait MessageHandler: Sized {
    type RecoverableError: Error;
    type IrrecoverableError: Error = !;

    async fn handle<S: MessageStream>(self, stream: S) -> Result<Self, HandlerError<Self>>;
}

#[cfg(test)]
mod tests {}
