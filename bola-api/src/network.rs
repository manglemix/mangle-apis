use mangle_api_core::serde::{Serialize, Deserialize, self};

#[derive(Clone, Deserialize, Serialize)]
#[serde(crate = "serde")]
pub enum NetworkMessage {
    HighscoreUpdate{ difficulty: String, score: u16 }
}