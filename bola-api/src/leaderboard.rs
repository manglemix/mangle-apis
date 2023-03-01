use std::{ops::Deref, sync::Arc, time::Instant};

use anyhow::{anyhow, Context};
use aws_sdk_dynamodb::model::{AttributeAction, AttributeValue, AttributeValueUpdate};
use derive_more::{Display, Error};
use mangle_api_core::{distributed::Node, log::error, parking_lot::RwLock};
use serde::Serialize;
use tokio::{
    spawn,
    sync::broadcast::{channel, Sender},
};

use crate::{
    db::DB,
    network::{HighscoreUpdate, NetworkMessage},
};

const LEADERBOARD_UPDATE_BUFFER_SIZE: usize = 8;

#[derive(Serialize)]
pub enum LeaderboardUpdate {
    #[serde(rename = "easy")]
    Easy(Vec<LeaderboardEntry>),
    #[serde(rename = "normal")]
    Normal(Vec<LeaderboardEntry>),
    #[serde(rename = "expert")]
    Expert(Vec<LeaderboardEntry>),
}

#[derive(PartialOrd, Ord, PartialEq, Eq, Clone, Serialize)]
pub struct LeaderboardEntry {
    pub score: u16,
    pub username: String,
}

struct LeaderboardImpl {
    easy_leaderboard: RwLock<Vec<LeaderboardEntry>>,
    normal_leaderboard: RwLock<Vec<LeaderboardEntry>>,
    expert_leaderboard: RwLock<Vec<LeaderboardEntry>>,

    last_update: RwLock<Instant>,

    leaderboard_span: usize,

    leaderboard_updater: Sender<Arc<LeaderboardUpdate>>,

    db: DB,
    node: Node<NetworkMessage>,
}

#[derive(Error, Display, Debug)]
pub enum AddLeaderboardEntryError {
    InternalError,
}

#[derive(Serialize)]
pub struct LeaderboardView {
    easy: Vec<LeaderboardEntry>,
    normal: Vec<LeaderboardEntry>,
    expert: Vec<LeaderboardEntry>,
}

#[derive(Clone)]
pub struct Leaderboard {
    inner: Arc<LeaderboardImpl>,
}

impl Leaderboard {
    async fn pull_leaderboard(
        db: &DB,
        leaderboard_name: &str,
        leaderboard_span: usize,
    ) -> Result<Vec<LeaderboardEntry>, anyhow::Error> {
        let query = db
            .client
            .query()
            .table_name(db.bola_profiles_table.clone())
            .index_name(format!("unused-{leaderboard_name}-index"))
            .key_condition_expression("unused = :partitionkeyval")
            .expression_attribute_values(":partitionkeyval", AttributeValue::N("0".into()))
            .scan_index_forward(false)
            .limit(leaderboard_span as i32)
            .send()
            .await?;

        let items = query
            .items()
            .ok_or(anyhow!("No items in easy_highscore query"))?;
        let mut leaderboard = Vec::with_capacity(leaderboard_span);

        for record in items {
            let email = record
                .get("email")
                .ok_or(anyhow!("No email in {leaderboard_name}"))?
                .as_s()
                .map_err(|e| anyhow!("email is not a string {e:?}"))?
                .clone();

            let score = record
                .get(leaderboard_name)
                .ok_or(anyhow!("No score in {leaderboard_name} for {email}"))?
                .as_n()
                .map_err(|e| anyhow!("score is not a number {e:?} for {email}"))?
                .parse()
                .context(format!("Parsing score in {leaderboard_name} for {email}"))?;

            let username = record
                .get("username")
                .ok_or(anyhow!("No username in {leaderboard_name} for {email}"))?
                .as_s()
                .map_err(|e| anyhow!("username is not a string {e:?} for {email}"))?
                .clone();

            leaderboard.push(LeaderboardEntry { score, username });
        }

        Ok(leaderboard)
    }

    pub async fn new(
        db: DB,
        node: Node<NetworkMessage>,
        leaderboard_span: usize,
    ) -> Result<Self, anyhow::Error> {
        let inner = Arc::new(LeaderboardImpl {
            easy_leaderboard: RwLock::new(
                Self::pull_leaderboard(&db, "easy_highscore", leaderboard_span).await?,
            ),
            normal_leaderboard: RwLock::new(
                Self::pull_leaderboard(&db, "normal_highscore", leaderboard_span).await?,
            ),
            expert_leaderboard: RwLock::new(
                Self::pull_leaderboard(&db, "expert_highscore", leaderboard_span).await?,
            ),
            last_update: RwLock::new(Instant::now()),
            leaderboard_span,
            leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
            db,
            node: node.clone(),
        });
        let this = Self { inner };
        let this2 = this.clone();
        let mut subscription = node.get_message_router().subscribe_to_highscore_update();

        spawn(async move {
            loop {
                let Some(msg) = subscription.wait_for_update().await else {
                    break
                };

                match msg.difficulty.as_str() {
                    "easy" => this.local_update_leaderboard(
                        &this.inner.easy_leaderboard,
                        LeaderboardEntry {
                            score: msg.score,
                            username: msg.username,
                        },
                        LeaderboardUpdate::Easy,
                    ),
                    "normal" => this.local_update_leaderboard(
                        &this.inner.normal_leaderboard,
                        LeaderboardEntry {
                            score: msg.score,
                            username: msg.username,
                        },
                        LeaderboardUpdate::Normal,
                    ),
                    "expert" => this.local_update_leaderboard(
                        &this.inner.expert_leaderboard,
                        LeaderboardEntry {
                            score: msg.score,
                            username: msg.username,
                        },
                        LeaderboardUpdate::Expert,
                    ),
                    s => {
                        error!(target: "leaderboard", "Found unexpected leaderboard_difficulty: {s}");
                        continue;
                    }
                };
            }
        });

        Ok(this2)
    }

    fn local_update_leaderboard(
        &self,
        leaderboard: &RwLock<Vec<LeaderboardEntry>>,
        entry: LeaderboardEntry,
        update_fn: impl Fn(Vec<LeaderboardEntry>) -> LeaderboardUpdate,
    ) -> bool {
        let mut leaderboard_writer = leaderboard.write();

        macro_rules! update {
            () => {{
                *self.inner.last_update.write() = Instant::now();
                leaderboard_writer.sort_by(|a, b| b.cmp(a));

                let _ = self
                    .inner
                    .leaderboard_updater
                    .send(Arc::new((update_fn)(leaderboard_writer.clone())));

                return true;
            }};
        }

        if leaderboard_writer.is_empty() {
            leaderboard_writer.push(entry);
            update!()
        }

        let mut entry = Some(entry);
        for saved_entry in leaderboard_writer.iter_mut() {
            let entry_ref = entry.as_ref().unwrap();

            if saved_entry.username == entry_ref.username {
                if saved_entry.score >= entry_ref.score {
                    return false;
                }

                *saved_entry = entry.unwrap();
                entry = None;
                break;
            }
        }
        let Some(entry) = entry else { update!() };

        if leaderboard_writer.len() >= self.inner.leaderboard_span {
            let last = leaderboard_writer.last_mut().unwrap();
            if *last >= entry {
                return false;
            }
            *last = entry;
        } else {
            leaderboard_writer.push(entry);
        }

        update!()
    }

    async fn add_leaderboard_entry(
        &self,
        leaderboard: &RwLock<Vec<LeaderboardEntry>>,
        email: String,
        entry: LeaderboardEntry,
        leaderboard_difficulty: &str,
        update_fn: impl Fn(Vec<LeaderboardEntry>) -> LeaderboardUpdate,
    ) -> Result<(), AddLeaderboardEntryError> {
        assert!(matches!(
            leaderboard_difficulty,
            "easy" | "normal" | "expert"
        ));

        if let Err(e) = self
            .inner
            .db
            .client
            .update_item()
            .table_name(self.inner.db.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email.clone()))
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
            error!(target: "leaderboard", "Error updating item for {}: {e:?}", email);
            return Err(AddLeaderboardEntryError::InternalError);
        }

        if !self.local_update_leaderboard(leaderboard, entry.clone(), update_fn) {
            return Ok(());
        };

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
        email: String,
        entry: LeaderboardEntry,
    ) -> Result<(), AddLeaderboardEntryError> {
        self.add_leaderboard_entry(
            &self.inner.easy_leaderboard,
            email,
            entry,
            "easy",
            LeaderboardUpdate::Easy,
        )
        .await
    }
    pub async fn add_normal_entry(
        &self,
        email: String,
        entry: LeaderboardEntry,
    ) -> Result<(), AddLeaderboardEntryError> {
        self.add_leaderboard_entry(
            &self.inner.normal_leaderboard,
            email,
            entry,
            "normal",
            LeaderboardUpdate::Normal,
        )
        .await
    }
    pub async fn add_expert_entry(
        &self,
        email: String,
        entry: LeaderboardEntry,
    ) -> Result<(), AddLeaderboardEntryError> {
        self.add_leaderboard_entry(
            &self.inner.expert_leaderboard,
            email,
            entry,
            "expert",
            LeaderboardUpdate::Expert,
        )
        .await
    }
    pub fn get_leaderboard(&self) -> LeaderboardView {
        LeaderboardView {
            easy: self.inner.easy_leaderboard.read().clone(),
            normal: self.inner.normal_leaderboard.read().clone(),
            expert: self.inner.expert_leaderboard.read().clone(),
        }
    }
    pub fn get_leaderboard_since(&self, since: Instant) -> Option<LeaderboardView> {
        if &since < self.inner.last_update.read().deref() {
            Some(self.get_leaderboard())
        } else {
            None
        }
    }
    pub async fn wait_for_update(&self) -> Option<Arc<LeaderboardUpdate>> {
        self.inner.leaderboard_updater.subscribe().recv().await.ok()
    }
}
