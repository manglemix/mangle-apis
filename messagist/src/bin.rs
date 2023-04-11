use async_bincode::tokio::{AsyncBincodeReader, AsyncBincodeWriter};
use async_trait::async_trait;
use bincode::ErrorKind;
// use bincode::ErrorKind;
use futures::{SinkExt, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{MessageStream, RecvError};

pub struct BinaryMessageStream<T: AsyncRead + AsyncWrite + Unpin + Send>(T);

#[async_trait]
impl<T> MessageStream for BinaryMessageStream<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send,
{
    type DeserializeError = bincode::Error;

    async fn recv_message<M>(mut self) -> Result<(M, Self), RecvError<Self>>
    where
        M: DeserializeOwned + Send,
    {
        let mut reader = AsyncBincodeReader::from(self.0);
        match reader.next().await {
            Some(Ok(msg)) => {
                self.0 = reader.into_inner();
                Ok((msg, self))
            }
            None => Err(RecvError::IOError(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Connection Closed",
            ))),
            Some(Err(e)) => {
                self.0 = reader.into_inner();
                Err(RecvError::DeserializeError(e, self))
            }
        }
        // let mut final_buf = vec![];
        // loop {
        //     let size: usize = self.0.read_u16().await.map_err(RecvError::IOError)? as usize;
        //     let mut buf = vec![0; size];
        //     self.0
        //         .read_exact(&mut buf)
        //         .await
        //         .map_err(RecvError::IOError)?;
        //     final_buf.append(&mut buf);

        //     if size < u16::MAX as usize {
        //         break;
        //     }
        // }

        // match bincode::deserialize(&final_buf) {
        //     Ok(msg) => Ok((msg, self)),
        //     Err(e) => return Err(RecvError::DeserializeError(e, self)),
        // }
    }

    async fn recv_borrowed_message<M, F, O>(mut self, f: F) -> Result<(O, Self), RecvError<Self>>
    where
        for<'a> M: Deserialize<'a> + Send,
        F: FnOnce(M) -> O + Send,
    {
        let mut reader = AsyncBincodeReader::from(self.0);
        match reader.next().await {
            Some(Ok(msg)) => {
                self.0 = reader.into_inner();
                Ok(((f)(msg), self))
            }
            None => Err(RecvError::IOError(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "Connection Closed",
            ))),
            Some(Err(e)) => {
                self.0 = reader.into_inner();
                Err(RecvError::DeserializeError(e, self))
            }
        }
        // let mut final_buf = vec![];
        // loop {
        //     let size: usize = self.0.read_u16().await.map_err(RecvError::IOError)? as usize;
        //     let mut buf = vec![0; size];
        //     self.0
        //         .read_exact(&mut buf)
        //         .await
        //         .map_err(RecvError::IOError)?;
        //     final_buf.append(&mut buf);

        //     if size < u16::MAX as usize {
        //         break;
        //     }
        // }

        // match bincode::deserialize(&final_buf) {
        //     Ok(msg) => Ok(((f)(msg), self)),
        //     Err(e) => return Err(RecvError::DeserializeError(e, self)),
        // }
    }

    async fn send_message<M: Serialize + Sync>(mut self, msg: &M) -> Result<Self, std::io::Error> {
        let mut writer = AsyncBincodeWriter::from(self.0);
        if let Err(e) = writer.send(msg).await {
            return match Box::into_inner(e) {
                ErrorKind::Io(e) => Err(e),
                _ => unreachable!(),
            };
        }
        if let Err(e) = writer.flush().await {
            return match Box::into_inner(e) {
                ErrorKind::Io(e) => Err(e),
                _ => unreachable!(),
            };
        }
        self.0 = writer.into_inner();
        Ok(self)
        // let mut bytes: VecDeque<u8> = bincode::serialize(&msg).unwrap().into();

        // loop {
        //     if bytes.len() >= u16::MAX as usize {
        //         self.0.write_u16(u16::MAX).await?;
        //         let tmp = bytes.split_off(u16::MAX as usize);
        //         let to_send = replace(&mut bytes, tmp);
        //         self.0.write_all(to_send.as_slices().0).await?;
        //     } else {
        //         self.0.write_u16(bytes.len() as u16).await?;
        //         self.0.write_all(bytes.as_slices().0).await?;
        //         break;
        //     }
        // }

        // Ok(self)
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> BinaryMessageStream<T> {
    pub async fn into_inner(self) -> T {
        self.0
    }
}

impl<T: AsyncRead + AsyncWrite + Unpin + Send> From<T> for BinaryMessageStream<T> {
    fn from(value: T) -> Self {
        BinaryMessageStream(value)
    }
}
