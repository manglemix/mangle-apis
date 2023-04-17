// #![feature(associated_type_defaults)]
// #![feature(never_type)]
#![feature(associated_type_bounds)]
// #![feature(box_into_inner)]

use std::{pin::Pin, any::Any, ops::DerefMut};

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize, Deserialize, Deserializer};

#[cfg(feature = "bincode")]
pub mod bin;
#[cfg(feature = "pipes")]
pub mod pipes;
#[cfg(feature = "json")]
pub mod text;


#[async_trait]
pub trait MessageStream: Sized + Send {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn recv_message<T>(&mut self) -> Result<T, Self::Error>
    where
        T: DeserializeOwned + Send + 'static;

    async fn send_message<T: Serialize + Send + Sync + 'static>(
        &mut self,
        msg: &T,
    ) -> Result<(), Self::Error>;
    async fn wait_for_error(&mut self) -> Self::Error;
}

#[async_trait]
pub trait AliasableMessageHandler: Sized {
    type Error;

    async fn handle<S: MessageStream>(&self, stream: S) -> Result<(), Self::Error>;
}

#[async_trait]
pub trait ExclusiveMessageHandler: Sized {
    type Error;

    async fn handle<S: MessageStream>(&mut self, stream: S) -> Result<(), Self::Error>;
}

#[async_trait]
impl<H: AliasableMessageHandler + Send + Sync> ExclusiveMessageHandler for H {
    type Error = <H as AliasableMessageHandler>::Error;

    async fn handle<S: MessageStream + Send>(&mut self, stream: S) -> Result<(), Self::Error> {
        <H as AliasableMessageHandler>::handle(self, stream).await
    }
}

#[cfg(test)]
mod tests {}
