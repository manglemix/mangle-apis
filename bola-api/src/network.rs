use axum::async_trait;
use derive_more::From;
use log::error;
use mangle_api_core::distributed::ServerName;
use messagist::{ExclusiveMessageHandler, MessageStream};
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

#[derive(Clone)]
pub struct SiblingNetworkHandler {
    highscore_updater: Sender<HighscoreUpdate>,
}

impl SiblingNetworkHandler {
    pub fn new() -> Self {
        Self {
            highscore_updater: channel(MESSAGE_ROUTER_BUFFER_SIZE).0,
        }
    }
}

#[async_trait]
impl ExclusiveMessageHandler for SiblingNetworkHandler {
    type SessionState = ServerName;

    async fn handle<S: MessageStream>(&mut self, mut stream: S, server_name: Self::SessionState) {
        let server_name = server_name.0;
        match stream.recv_message().await {
            Ok(NetworkMessage::HighscoreUpdate(msg)) => {
                let _ = self.highscore_updater.send(msg);
            }
            Err(e) => error!("Error receiving node message: {e} from {server_name}"),
        }
    }
}

impl SiblingNetworkHandler {
    pub fn subscribe_to_highscore_update(&self) -> HighScoreUpdateSubscription {
        HighScoreUpdateSubscription(self.highscore_updater.subscribe())
    }
}

#[derive(Clone, Deserialize, Serialize, From)]
pub enum NetworkMessage {
    HighscoreUpdate(HighscoreUpdate),
}
