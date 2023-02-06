use std::error::Error;

use persy::{BeginTransactionError, PE, PrepareError, IndexOpsError};


pub enum WriteError {
    BeginTransactionError(PE<BeginTransactionError>),
    SerializeError(bincode::Error),
    InsertError(Box<dyn Error>),
    PrepareError(PE<PrepareError>),
    UnrecognizedNamespace
}


impl From<PE<BeginTransactionError>> for WriteError {
    fn from(value: PE<BeginTransactionError>) -> Self {
        Self::BeginTransactionError(value)
    }
}


impl From<PE<PrepareError>> for WriteError {
    fn from(value: PE<PrepareError>) -> Self {
        Self::PrepareError(value)
    }
}


impl From<bincode::Error> for WriteError {
    fn from(value: bincode::Error) -> Self {
        Self::SerializeError(value)
    }
}


impl WriteError {
    pub(crate) fn insert_error(e: impl Error + 'static) -> Self {
        Self::InsertError(Box::new(e))
    }
}


pub enum ReadError {
    IndexOpsError(PE<IndexOpsError>),
    UnrecognizedNamespace,
    DeserializeError(bincode::Error)
}


impl From<bincode::Error> for ReadError {
    fn from(value: bincode::Error) -> Self {
        Self::DeserializeError(value)
    }
}


impl From<PE<IndexOpsError>> for ReadError {
    fn from(value: PE<IndexOpsError>) -> Self {
        Self::IndexOpsError(value)
    }
}


pub enum OwnError {
    UnrecognizedNamespace,
    UnrecognizedName,
    IndexOpsError(PE<IndexOpsError>)
}


impl From<PE<IndexOpsError>> for OwnError {
    fn from(value: PE<IndexOpsError>) -> Self {
        Self::IndexOpsError(value)
    }
}