use bincode::{serialize, deserialize};
use serde::{Serialize, de::DeserializeOwned};

use super::error::ReadError;

pub trait ToBytes {
    fn to_bytes(self) -> Vec<u8>;
}


impl<T: Serialize> ToBytes for T {
    fn to_bytes(self) -> Vec<u8> {
        serialize(&self).expect("Serialize data")
    }
}


pub trait FromBytes: Sized {
    type Error: Into<ReadError>;

    fn from_bytes(data: Vec<u8>) -> Result<Self, Self::Error>;
    fn from_slice(data: &[u8]) -> Result<Self, Self::Error>;
}


impl<T: DeserializeOwned> FromBytes for T {
    type Error = bincode::Error;

    fn from_bytes(data: Vec<u8>) -> Result<Self, Self::Error> {
        deserialize(data.as_slice())
    }
    fn from_slice(data: &[u8]) -> Result<Self, Self::Error> {
        deserialize(data)
    }
}