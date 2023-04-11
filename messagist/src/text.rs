use std::{collections::VecDeque, mem::replace};

use async_trait::async_trait;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufStream};

use crate::{MessageStream, RecvError};

#[async_trait]
pub trait TextStream: Sized {
    async fn recv_string(mut self) -> Result<(Vec<u8>, Self), std::io::Error>;
    async fn send_string(mut self, msg: String) -> Result<Self, std::io::Error>;
}

pub struct BinaryTextStream<T: AsyncRead + AsyncWrite + Unpin + Send>(BufStream<T>);

pub struct JsonMessageStream<T>(T);

#[async_trait]
impl<S: AsyncRead + AsyncWrite + Unpin + Send> MessageStream
    for JsonMessageStream<BinaryTextStream<S>>
{
    type DeserializeError = serde_json::Error;

    async fn recv_message<T>(mut self) -> Result<(T, Self), RecvError<Self>>
    where
        T: DeserializeOwned,
    {
        let mut final_buf = vec![];
        loop {
            let size: usize = self.0 .0.read_u16().await.map_err(RecvError::IOError)? as usize;
            let mut buf = vec![0; size];
            self.0
                 .0
                .read_exact(&mut buf)
                .await
                .map_err(RecvError::IOError)?;
            final_buf.append(&mut buf);

            if size < u16::MAX as usize {
                break;
            }
        }

        match serde_json::from_slice(&final_buf) {
            Ok(msg) => Ok((msg, self)),
            Err(e) => return Err(RecvError::DeserializeError(e, self)),
        }
    }

    async fn recv_borrowed_message<T, F, O>(mut self, f: F) -> Result<(O, Self), RecvError<Self>>
    where
        for<'a> T: Deserialize<'a>,
        F: FnOnce(T) -> O + Send,
    {
        let mut final_buf = vec![];
        loop {
            let size: usize = self.0 .0.read_u16().await.map_err(RecvError::IOError)? as usize;
            let mut buf = vec![0; size];
            self.0
                 .0
                .read_exact(&mut buf)
                .await
                .map_err(RecvError::IOError)?;
            final_buf.append(&mut buf);

            if size < u16::MAX as usize {
                break;
            }
        }

        match serde_json::from_slice(&final_buf) {
            Ok(msg) => Ok(((f)(msg), self)),
            Err(e) => return Err(RecvError::DeserializeError(e, self)),
        }
    }

    async fn send_message<T: Serialize + Sync>(mut self, msg: &T) -> Result<Self, std::io::Error> {
        let mut bytes: VecDeque<u8> = serde_json::to_vec(&msg).unwrap().into();

        loop {
            if bytes.len() >= u16::MAX as usize {
                self.0 .0.write_u16(u16::MAX).await?;
                let tmp = bytes.split_off(u16::MAX as usize);
                let to_send = replace(&mut bytes, tmp);
                self.0 .0.write_all(to_send.as_slices().0).await?;
            } else {
                self.0 .0.write_u16(bytes.len() as u16).await?;
                self.0 .0.write_all(bytes.as_slices().0).await?;
                break;
            }
        }

        Ok(self)
    }
}

#[async_trait]
impl<S: TextStream + Send> MessageStream for JsonMessageStream<S> {
    type DeserializeError = serde_json::Error;

    async fn recv_message<T>(mut self) -> Result<(T, Self), RecvError<Self>>
    where
        T: DeserializeOwned,
    {
        let (msg, tmp) = self.0.recv_string().await.map_err(RecvError::IOError)?;
        self.0 = tmp;

        match serde_json::from_slice(&msg) {
            Ok(msg) => Ok((msg, self)),
            Err(e) => return Err(RecvError::DeserializeError(e, self)),
        }
    }

    async fn recv_borrowed_message<T, F, O>(mut self, f: F) -> Result<(O, Self), RecvError<Self>>
    where
        for<'a> T: Deserialize<'a>,
        F: FnOnce(T) -> O + Send,
    {
        let (msg, tmp) = self.0.recv_string().await.map_err(RecvError::IOError)?;
        self.0 = tmp;

        match serde_json::from_slice(&msg) {
            Ok(msg) => Ok(((f)(msg), self)),
            Err(e) => return Err(RecvError::DeserializeError(e, self)),
        }
    }

    async fn send_message<T: Serialize + Sync>(mut self, msg: &T) -> Result<Self, std::io::Error> {
        let tmp = self
            .0
            .send_string(serde_json::to_string(&msg).unwrap().into())
            .await?;
        self.0 = tmp;
        Ok(self)
    }
}

impl<S> From<S> for JsonMessageStream<S> {
    fn from(value: S) -> Self {
        Self(value)
    }
}
