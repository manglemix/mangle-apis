use std::sync::Arc;

use aws_sdk_dynamodb::model::{AttributeAction, AttributeValue, AttributeValueUpdate};
use mangle_api_core::{
    anyhow,
    distributed::Node,
    log::error,
    parking_lot::RwLock,
    tokio::sync::broadcast::{channel, Sender},
};

use crate::{
    db::DB,
    network::{HighscoreUpdate, NetworkMessage},
};

const LEADERBOARD_UPDATE_BUFFER_SIZE: usize = 8;

#[derive(Clone)]
pub struct LeaderboardUpdate {
    pub leaderboard: Arc<Vec<LeaderboardEntry>>,
}

#[derive(PartialOrd, PartialEq, Ord, Eq, Clone)]
pub struct LeaderboardEntry {
    score: u16,
    username: String,
}

struct LeaderboardImpl {
    easy_leaderboard: RwLock<Vec<LeaderboardEntry>>,
    normal_leaderboard: RwLock<Vec<LeaderboardEntry>>,
    expert_leaderboard: RwLock<Vec<LeaderboardEntry>>,
    leaderboard_span: usize,
    easy_leaderboard_updater: Sender<LeaderboardUpdate>,
    normal_leaderboard_updater: Sender<LeaderboardUpdate>,
    expert_leaderboard_updater: Sender<LeaderboardUpdate>,
    db: DB,
    node: Node<NetworkMessage>,
}

pub enum AddLeaderboardEntryError {
    NotAUser,
    InternalError,
}

#[derive(Clone)]
pub struct Leaderboard {
    inner: Arc<LeaderboardImpl>,
}

impl Leaderboard {
    pub async fn new(
        db: DB,
        node: Node<NetworkMessage>,
        leaderboard_span: usize,
    ) -> Result<Self, anyhow::Error> {
        Ok(Self {
            inner: Arc::new(LeaderboardImpl {
                easy_leaderboard: Default::default(),
                normal_leaderboard: Default::default(),
                expert_leaderboard: Default::default(),
                leaderboard_span,
                easy_leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
                normal_leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
                expert_leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
                db,
                node,
            }),
        })
    }

    async fn add_leaderboard_entry(
        &self,
        leaderboard: &RwLock<Vec<LeaderboardEntry>>,
        updater: &Sender<LeaderboardUpdate>,
        entry: LeaderboardEntry,
        leaderboard_difficulty: &str,
    ) -> Result<(), AddLeaderboardEntryError> {
        assert!(matches!(
            leaderboard_difficulty,
            "easy" | "normal" | "expert"
        ));

        let mut leaderboard_writer = leaderboard.write();

        let Err(idx) = leaderboard_writer.binary_search(&entry) else {
            return Ok(())
        };

        leaderboard_writer.insert(idx, entry.clone());

        if leaderboard_writer.len() > self.inner.leaderboard_span {
            leaderboard_writer.pop();
        }

        let _ = updater.send(LeaderboardUpdate {
            leaderboard: Arc::new(leaderboard_writer.clone()),
        });

        let email = match self.inner.db.get_email_from_username(&entry.username).await {
            Ok(Some(x)) => x,
            Ok(None) => return Err(AddLeaderboardEntryError::NotAUser),
            Err(e) => {
                error!(target: "leaderboard", "Error getting email for {}: {e:?}", entry.username);
                return Err(AddLeaderboardEntryError::InternalError);
            }
        };

        if let Err(e) = self
            .inner
            .db
            .client
            .update_item()
            .table_name(self.inner.db.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email))
            .attribute_updates(
                format!("{leaderboard_difficulty}_highscore"),
                AttributeValueUpdate::builder()
                    .action(AttributeAction::Put)
                    .value(AttributeValue::N(entry.score.to_string()))
                    .build(),
            )
            .send()
            .await
        {
            error!(target: "leaderboard", "Error updating item for {}: {e:?}", entry.username);
            return Err(AddLeaderboardEntryError::InternalError);
        }

        for (domain, err) in self
            .inner
            .node
            .broadcast_message(&NetworkMessage::HighscoreUpdate(HighscoreUpdate {
                username: entry.username,
                difficulty: leaderboard_difficulty.into(),
                score: entry.score,
            }))
            .await
        {
            error!(target: "leaderboard", "Error broadcasting message to {}: {:?}", domain, err);
        }

        Ok(())
    }
    pub async fn add_easy_entry(
        &self,
        entry: LeaderboardEntry,
    ) -> Result<(), AddLeaderboardEntryError> {
        self.add_leaderboard_entry(
            &self.inner.easy_leaderboard,
            &self.inner.easy_leaderboard_updater,
            entry,
            "easy",
        )
        .await
    }
    pub async fn add_normal_entry(
        &self,
        entry: LeaderboardEntry,
    ) -> Result<(), AddLeaderboardEntryError> {
        self.add_leaderboard_entry(
            &self.inner.normal_leaderboard,
            &self.inner.normal_leaderboard_updater,
            entry,
            "normal",
        )
        .await
    }
    pub async fn add_expert_entry(
        &self,
        entry: LeaderboardEntry,
    ) -> Result<(), AddLeaderboardEntryError> {
        self.add_leaderboard_entry(
            &self.inner.expert_leaderboard,
            &self.inner.expert_leaderboard_updater,
            entry,
            "expert",
        )
        .await
    }
    pub async fn wait_for_easy_update(&self) -> Option<LeaderboardUpdate> {
        self.inner
            .easy_leaderboard_updater
            .subscribe()
            .recv()
            .await
            .ok()
    }
    pub async fn wait_for_normal_update(&self) -> Option<LeaderboardUpdate> {
        self.inner
            .normal_leaderboard_updater
            .subscribe()
            .recv()
            .await
            .ok()
    }
    pub async fn wait_for_expert_update(&self) -> Option<LeaderboardUpdate> {
        self.inner
            .expert_leaderboard_updater
            .subscribe()
            .recv()
            .await
            .ok()
    }
}
