use std::{ops::Deref, sync::Arc};

use anyhow::{anyhow, Context};
use aws_sdk_dynamodb::model::{AttributeAction, AttributeValue, AttributeValueUpdate};
use axum::{
    extract::{State, WebSocketUpgrade},
    response::Response,
};
use derive_more::{Display, Error};
use mangle_api_core::{
    distributed::Node, log::error, parking_lot::RwLock, serde_json, ws::ManagedWebSocket,
};
use serde::Serialize;
use tokio::{
    select, spawn,
    sync::broadcast::{channel, Sender},
};

use crate::{
    db::DB,
    network::{HighscoreUpdate, NetworkMessage},
    WS_PING_DELAY,
};

const LEADERBOARD_UPDATE_BUFFER_SIZE: usize = 8;

#[derive(Clone)]
pub struct LeaderboardUpdate {
    pub leaderboard: Arc<Vec<LeaderboardEntry>>,
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
    leaderboard_span: usize,
    easy_leaderboard_updater: Sender<LeaderboardUpdate>,
    normal_leaderboard_updater: Sender<LeaderboardUpdate>,
    expert_leaderboard_updater: Sender<LeaderboardUpdate>,
    db: DB,
    node: Node<NetworkMessage>,
}

#[derive(Error, Display, Debug)]
pub enum AddLeaderboardEntryError {
    InternalError,
}

#[derive(Clone)]
pub struct Leaderboard {
    inner: Arc<LeaderboardImpl>,
}

impl Leaderboard {
    async fn get_leaderboard(
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
        let inner2 = Arc::new(LeaderboardImpl {
            easy_leaderboard: RwLock::new(
                Self::get_leaderboard(&db, "easy_highscore", leaderboard_span).await?,
            ),
            normal_leaderboard: RwLock::new(
                Self::get_leaderboard(&db, "normal_highscore", leaderboard_span).await?,
            ),
            expert_leaderboard: RwLock::new(
                Self::get_leaderboard(&db, "expert_highscore", leaderboard_span).await?,
            ),
            leaderboard_span,
            easy_leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
            normal_leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
            expert_leaderboard_updater: channel(LEADERBOARD_UPDATE_BUFFER_SIZE).0,
            db,
            node: node.clone(),
        });
        let inner = inner2.clone();
        let mut subscription = node.get_message_router().subscribe_to_highscore_update();

        spawn(async move {
            loop {
                let Some(msg) = subscription.wait_for_update().await else {
                    break
                };

                let (leaderboard, updater) = match msg.difficulty.as_str() {
                    "easy" => (&inner.easy_leaderboard, &inner.easy_leaderboard_updater),
                    "normal" => (&inner.normal_leaderboard, &inner.normal_leaderboard_updater),
                    "expert" => (&inner.expert_leaderboard, &inner.expert_leaderboard_updater),
                    s => {
                        error!(target: "leaderboard", "Found unexpected leaderboard_difficulty: {s}");
                        continue;
                    }
                };

                Self::local_update_leaderboard(
                    leaderboard_span,
                    leaderboard,
                    LeaderboardEntry {
                        score: msg.score,
                        username: msg.username,
                    },
                    updater,
                );
            }
        });

        Ok(Self { inner: inner2 })
    }

    fn local_update_leaderboard(
        leaderboard_span: usize,
        leaderboard: &RwLock<Vec<LeaderboardEntry>>,
        entry: LeaderboardEntry,
        updater: &Sender<LeaderboardUpdate>,
    ) -> bool {
        let mut leaderboard_writer = leaderboard.write();

        macro_rules! update {
            () => {{
                leaderboard_writer.sort_by(|a, b| b.cmp(a));

                let _ = updater.send(LeaderboardUpdate {
                    leaderboard: Arc::new(leaderboard_writer.clone()),
                });

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

        if leaderboard_writer.len() >= leaderboard_span {
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
        updater: &Sender<LeaderboardUpdate>,
        email: String,
        entry: LeaderboardEntry,
        leaderboard_difficulty: &str,
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

        if !Self::local_update_leaderboard(
            self.inner.leaderboard_span,
            leaderboard,
            entry.clone(),
            updater,
        ) {
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
            &self.inner.easy_leaderboard_updater,
            email,
            entry,
            "easy",
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
            &self.inner.normal_leaderboard_updater,
            email,
            entry,
            "normal",
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
            &self.inner.expert_leaderboard_updater,
            email,
            entry,
            "expert",
        )
        .await
    }
    pub fn get_easy_leaderboard(&self) -> Vec<LeaderboardEntry> {
        self.inner.easy_leaderboard.read().clone()
    }
    pub fn get_normal_leaderboard(&self) -> Vec<LeaderboardEntry> {
        self.inner.normal_leaderboard.read().clone()
    }
    pub fn get_expert_leaderboard(&self) -> Vec<LeaderboardEntry> {
        self.inner.expert_leaderboard.read().clone()
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

macro_rules! get_leaderboard {
    ($ws: expr, $leaderboard: expr, $get_fn: ident, $wait_fn: ident) => {
        $ws.on_upgrade(|ws| async move {
            let ws = ManagedWebSocket::new(ws, WS_PING_DELAY);
            let leaderboard = $leaderboard.$get_fn();
            let mut ws =
                ws.send(serde_json::to_string(&leaderboard).expect("leaderboard to serialize"));

            if ws.is_none() {
                return;
            }

            loop {
                select! {
                    () = ManagedWebSocket::loop_recv(&mut ws, None) => {
                        break
                    }
                    opt = $leaderboard.$wait_fn() => {
                        if let Some(LeaderboardUpdate{ leaderboard }) = opt {
                            ws = ws.unwrap().send(
                                serde_json::to_string(leaderboard.deref())
                                    .expect("leaderboard to serialize")
                            );
                        } else {
                            break
                        }
                    }
                }
            }
        })
    };
}

#[axum::debug_handler]
pub(crate) async fn get_easy_leaderboard(
    State(leaderboard): State<Leaderboard>,
    ws: WebSocketUpgrade,
) -> Response {
    get_leaderboard!(ws, leaderboard, get_easy_leaderboard, wait_for_easy_update)
}

#[axum::debug_handler]
pub(crate) async fn get_normal_leaderboard(
    State(leaderboard): State<Leaderboard>,
    ws: WebSocketUpgrade,
) -> Response {
    get_leaderboard!(
        ws,
        leaderboard,
        get_normal_leaderboard,
        wait_for_normal_update
    )
}

#[axum::debug_handler]
pub(crate) async fn get_expert_leaderboard(
    State(leaderboard): State<Leaderboard>,
    ws: WebSocketUpgrade,
) -> Response {
    get_leaderboard!(
        ws,
        leaderboard,
        get_expert_leaderboard,
        wait_for_expert_update
    )
}
