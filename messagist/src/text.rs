use std::pin::Pin;

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize, Deserialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{MessageStream, bin::BinaryError};


#[cfg(feature = "bin")]
use crate::bin::{BinaryMessageStream};

#[derive(thiserror::Error, Debug, derive_more::From)]
pub enum BinaryJsonError {
    #[error("IOError {0}")]
    IOError(std::io::Error),
    #[error("DeserializeError {0}")]
    DeserializeError(serde_json::Error)
}

#[async_trait]
pub trait TextStream: Sized {
    type Error: std::error::Error + Send + Sync + 'static;

    async fn recv_string(&mut self) -> Result<String, Self::Error>;
    async fn send_string(&mut self, msg: String) -> Result<(), Self::Error>;
    async fn wait_for_error(&mut self) -> Self::Error;
}

pub struct JsonMessageStream<T>(T);

#[cfg(feature = "bin")]
#[async_trait]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> MessageStream
    for JsonMessageStream<BinaryMessageStream<S>>
{
    type Error = BinaryJsonError;

    async fn recv_message<T>(&mut self) -> Result<T, Self::Error>
    where
        T: DeserializeOwned + Send + 'static,
    {
        let data: Vec<u8> = self.0
            .recv_message()
            .await
            .map_err(|e| match e {
                BinaryError::DeserializeError(_) => unreachable!(),
                BinaryError::IOError(e) => e
            })?;
        
        serde_json::from_slice(&data)
            .map_err(Into::into)
    }

    async fn send_message<T: Serialize + Send + Sync + 'static>(
        &mut self,
        msg: &T,
    ) -> Result<(), Self::Error> {
        self
            .0
            .send_message(&serde_json::to_vec(&msg).unwrap())
            .await
            .map_err(|e| match e {
                BinaryError::DeserializeError(_) => unreachable!(),
                BinaryError::IOError(e) => e.into()
            })
    }

    async fn wait_for_error(&mut self) -> Self::Error {
        let BinaryError::IOError(e) = self
            .0
            .wait_for_error()
            .await else {
                unreachable!()
            };
        e.into()
    }
}


#[derive(thiserror::Error, Debug)]
pub enum TextJsonError<E: std::error::Error> {
    #[error("TextError {0}")]
    TextError(E),
    #[error("DeserializeError {0}")]
    DeserializeError(serde_json::Error)
}


#[async_trait]
impl<S: TextStream<Error: Sync> + Send + Sync> MessageStream for JsonMessageStream<S> {
    type Error = TextJsonError<S::Error>;

    async fn recv_message<T>(&mut self) -> Result<T, Self::Error>
    where
        T: DeserializeOwned + Send,
    {
        let msg = self.0.recv_string().await.map_err(TextJsonError::TextError)?;
        serde_json::from_str(&msg).map_err(TextJsonError::DeserializeError)
    }

    async fn send_message<T: Serialize + Send + Sync + 'static>(
        &mut self,
        msg: &T,
    ) -> Result<(), Self::Error> {
        self.0
            .send_string(serde_json::to_string(&msg).unwrap())
            .await
            .map_err(TextJsonError::TextError)
    }

    async fn wait_for_error(&mut self) -> Self::Error {
        TextJsonError::TextError(self.0.wait_for_error().await)
    }
}

impl<S> From<S> for JsonMessageStream<S> {
    fn from(value: S) -> Self {
        Self(value)
    }
}
