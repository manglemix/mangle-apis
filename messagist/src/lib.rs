// #![feature(associated_type_defaults)]
// #![feature(never_type)]
#![feature(associated_type_bounds)]
// #![feature(box_into_inner)]

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};

#[cfg(feature = "bincode")]
pub mod bin;
#[cfg(feature = "pipes")]
pub mod pipes;
#[cfg(feature = "json")]
pub mod text;

pub enum Ref<'a, T> {
    Owned(T),
    Borrowed(&'a T),
}

impl<'a, T> Ref<'a, T> {
    pub fn get_ref(&self) -> &T {
        match self {
            Ref::Owned(x) => x,
            Ref::Borrowed(x) => x,
        }
    }
}

impl<'a, T> From<T> for Ref<'a, T> {
    fn from(value: T) -> Self {
        Self::Owned(value)
    }
}

impl<'a, T> From<&'a T> for Ref<'a, T> {
    fn from(value: &'a T) -> Self {
        Self::Borrowed(value)
    }
}

#[async_trait]
pub trait MessageStream: Sized + Send {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn recv_message<T>(&mut self) -> Result<T, Self::Error>
    where
        T: DeserializeOwned + Send + 'static;

    async fn send_message<T: Serialize + Send + Sync>(&mut self, msg: T)
        -> Result<(), Self::Error>;
    async fn wait_for_error(&mut self) -> Self::Error;
}

#[async_trait]
pub trait AliasableMessageHandler: Sized {
    type SessionState: Send;

    async fn handle<S: MessageStream>(&self, stream: S, session_state: Self::SessionState);
}

#[async_trait]
pub trait ExclusiveMessageHandler: Sized {
    type SessionState: Send;

    async fn handle<S: MessageStream>(&mut self, stream: S, session_state: Self::SessionState);
}

#[async_trait]
impl<H: AliasableMessageHandler + Send + Sync> ExclusiveMessageHandler for H {
    type SessionState = <H as AliasableMessageHandler>::SessionState;

    async fn handle<S: MessageStream + Send>(
        &mut self,
        stream: S,
        session_state: Self::SessionState,
    ) {
        <H as AliasableMessageHandler>::handle(self, stream, session_state).await;
    }
}

#[cfg(test)]
mod tests {}
