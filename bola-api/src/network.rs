use derive_more::From;
use mangle_api_core::distributed::{MessageRouter, NetworkMessageSet};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast::{channel, Receiver, Sender};

const MESSAGE_ROUTER_BUFFER_SIZE: usize = 8;

#[derive(Clone, Deserialize, Serialize)]
pub struct HighscoreUpdate {
    pub difficulty: String,
    pub username: String,
    pub score: u16,
}

pub struct HighScoreUpdateSubscription(Receiver<HighscoreUpdate>);

impl HighScoreUpdateSubscription {
    pub async fn wait_for_update(&mut self) -> Option<HighscoreUpdate> {
        self.0.recv().await.ok()
    }
}

pub struct NetworkMessageRouter {
    highscore_updater: Sender<HighscoreUpdate>,
}

impl MessageRouter<NetworkMessage> for NetworkMessageRouter {
    fn new() -> Self {
        Self {
            highscore_updater: channel(MESSAGE_ROUTER_BUFFER_SIZE).0,
        }
    }

    fn route_message(&self, _domain: &std::sync::Arc<str>, message: NetworkMessage) -> bool {
        match message {
            NetworkMessage::HighscoreUpdate(msg) => self.highscore_updater.send(msg).is_ok(),
        }
    }
}

impl NetworkMessageRouter {
    pub fn subscribe_to_highscore_update(&self) -> HighScoreUpdateSubscription {
        HighScoreUpdateSubscription(self.highscore_updater.subscribe())
    }
}

#[derive(Clone, Deserialize, Serialize, From)]
pub enum NetworkMessage {
    HighscoreUpdate(HighscoreUpdate),
}

impl NetworkMessageSet for NetworkMessage {
    type MessageRouter = NetworkMessageRouter;
}
