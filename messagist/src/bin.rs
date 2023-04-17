use async_trait::async_trait;
use serde::{de::DeserializeOwned, Serialize};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::{MessageStream};

pub struct BinaryMessageStream<T: AsyncRead + AsyncWrite + Unpin + Send>(pub(crate) T);


#[derive(thiserror::Error, Debug, derive_more::From)]
pub enum BinaryError {
    #[error("IOError {0}")]
    IOError(std::io::Error),
    #[error("DeserializeError {0}")]
    DeserializeError(bincode::Error)
}


#[async_trait]
impl<T> MessageStream for BinaryMessageStream<T>
where
    T: AsyncRead + AsyncWrite + Unpin + Send,
{
    type Error = BinaryError;

    async fn recv_message<M>(&mut self) -> Result<M, Self::Error>
    where
        M: DeserializeOwned + Send + 'static,
    {
        let mut size_byte_count = [0u8];
        self.0.read_exact(&mut size_byte_count).await?;
        let size_byte_count = size_byte_count[0] as usize;

        let usize_size = (usize::BITS / 8) as usize;

        if size_byte_count > usize_size {
            return Err(BinaryError::DeserializeError(Box::new(
                bincode::ErrorKind::SizeLimit,
            )));
        }

        let mut buf = vec![0; usize_size];
        let filled_half = buf.split_at_mut(size_byte_count).0;
        self.0.read_exact(filled_half).await?;
        let size = usize::from_le_bytes(buf.as_slice().try_into().unwrap());

        buf.resize(size, 0);
        self.0.read_exact(&mut buf).await?;

        bincode::deserialize(&buf).map_err(Into::into)
    }

    async fn send_message<M: Serialize + Send + Sync + 'static>(
        &mut self,
        msg: &M,
    ) -> Result<(), Self::Error> {
        let mut data = bincode::serialize(&msg).unwrap();
        let size = data.len();
        let mut size_vec = size.to_le_bytes().to_vec();

        // trim trailing zeroes
        for i in (0..size_vec.len()).rev() {
            if size_vec[i] > 0 {
                size_vec.resize(i + 1, 0);
                break;
            }
        }

        let mut final_buf = size_vec;
        final_buf.insert(0, final_buf.len() as u8);

        final_buf.append(&mut data);

        self.0.write_all(&final_buf).await?;
        Ok(())
    }

    async fn wait_for_error(&mut self) -> Self::Error {
        loop {
            let mut buf = [0; 16];
            let Err(e) = self.0.read(&mut buf).await else { continue };
            break e.into();
        }
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
