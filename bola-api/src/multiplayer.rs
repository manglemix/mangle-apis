use std::num::{NonZeroU16, TryFromIntError};

use mangle_api_core::rand::{thread_rng, Rng};
use mangle_api_core::webrtc::{RandomID, WebRTCSessionManager};

#[derive(PartialEq, Eq, Hash, Clone, Copy, derive_more::Display, Debug)]
pub struct RoomCode(NonZeroU16);

impl TryFrom<u16> for RoomCode {
    type Error = TryFromIntError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        NonZeroU16::try_from(value).map(|x| Self(x))
    }
}

impl RandomID for RoomCode {
    fn generate() -> Self {
        RoomCode(thread_rng().gen_range(1000..=9999).try_into().unwrap())
    }
}

pub type Multiplayer = WebRTCSessionManager<RoomCode>;
