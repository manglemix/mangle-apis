use std::{sync::Arc, ops::Deref};

use axum::{
    body::BoxBody,
    extract::{ws::Message, State, WebSocketUpgrade},
    http::{StatusCode, HeaderValue},
    response::Response,
};
use mangle_api_core::{
    auth::token::{HeaderTokenGranter, VerifiedToken},
    log::error,
    serde_json,
    ws::{ManagedWebSocket, WebSocketCode},
};
use rustrict::CensorStr;
use serde::Deserialize;
use tokio::select;

use crate::{
    db::UserProfile, leaderboard::LeaderboardEntry, GlobalState, LoginTokenData, LoginTokenGranter,
    WS_PING_DELAY,
};

#[axum::debug_handler(state = GlobalState)]
pub(crate) async fn login(State(globals): State<GlobalState>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|ws| async move {
        let db = &globals.db;
        let oidc = &globals.goidc.0;

        let (auth_url, fut) = oidc.initiate_auth(["openid", "email"]);
        let ws = ManagedWebSocket::new(ws, WS_PING_DELAY);

        let Some(ws) = ws.send(auth_url.to_string()) else { return };
        let mut ws = Some(ws);

        let opt = select! {
            opt = fut => { opt }
            () = ManagedWebSocket::loop_recv(&mut ws, None) => { return }
        };
        let mut ws = ws.unwrap();

        let Some(data) = opt else {
            ws.close_frame(WebSocketCode::Ok, "Auth Failed");
            return
        };
        let Some(email) = data.email else {
            ws.close_frame(WebSocketCode::BadPayload, "Auth Failed");
            return
        };

        let token;
        let token_data;

        match db.get_user_profile_by_email(&email).await {
            Ok(Some(profile)) => {
                let Some(ws_tmp) = ws.send(serde_json::to_string(&profile).unwrap()) else { return };
                let (token_tmp, token_data_tmp) = globals.login_tokens.create_token(LoginTokenData{
                    email: email.clone(),
                    username: profile.username
                });

                token = token_tmp;
                token_data = token_data_tmp;

                let Some(ws_tmp) = ws_tmp.send(
                    token.to_str().unwrap()
                ) else { return };

                ws = ws_tmp;
            }
            Ok(None) => {
                let Some(ws_tmp) = ws.send("Sign Up") else { return };
                ws = ws_tmp;
                let mut profile;

                loop {
                    let Some((ws_tmp, msg)) = ws.recv().await else { return };
                    ws = ws_tmp;

                    let Message::Text(msg) = msg else {
                        ws.close_frame(WebSocketCode::BadPayload, "Expected profile");
                        return
                    };

                    let Ok(tmp_profile) = serde_json::from_str::<UserProfile>(&msg) else {
                        ws.close_frame(WebSocketCode::BadPayload, "");
                        return
                    };
                    profile = tmp_profile;

                    if profile.username.is_inappropriate() {
                        let Some(ws_tmp) = ws.send("Inappropriate username") else {
                            return
                        };
                        ws = ws_tmp;
                        continue;
                    }

                    match db.is_username_taken(&profile.username).await {
                        Ok(true) => {
                            let Some(ws_tmp) = ws.send("Username already used") else {
                                return
                            };
                            ws = ws_tmp;
                            continue;
                        }
                        Ok(false) => {}
                        Err(e) => {
                            error!(target: "login", "{:?}", e.context("checking if username is taken"));
                            ws.close_frame(WebSocketCode::InternalError, "");
                            return;
                        }
                    };

                    break;
                }

                if let Err(e) = db.create_user_profile(
                    email.clone(),
                    profile.username.clone(),
                    profile.tournament_wins
                ).await {
                    error!(target: "login", "{:?}", e.context("creating user profile"));
                    ws.close_frame(WebSocketCode::InternalError, "");
                    return;
                }

                let leaderboard = &globals.leaderboard;

                if let Err(e) = leaderboard.add_easy_entry(email.clone(), LeaderboardEntry {
                    score: profile.easy_highscore,
                    username: profile.username.clone(),
                }).await {
                    let e = anyhow::Error::from(e);
                    error!(target: "login", "{:?}", e.context(format!("adding easy entry for {email}")));
                }
                if let Err(e) = leaderboard.add_normal_entry(email.clone(), LeaderboardEntry {
                    score: profile.normal_highscore,
                    username: profile.username.clone(),
                }).await {
                    let e = anyhow::Error::from(e);
                    error!(target: "login", "{:?}", e.context(format!("adding normal entry for {email}")));
                }
                if let Err(e) = leaderboard.add_expert_entry(email.clone(), LeaderboardEntry {
                    score: profile.easy_highscore,
                    username: profile.username.clone(),
                }).await {
                    let e = anyhow::Error::from(e);
                    error!(target: "login", "{:?}", e.context(format!("adding easy entry for {email}")));
                }

                let (token_tmp, token_data_tmp) = globals.login_tokens.create_token(LoginTokenData {
                    email,
                    username: profile.username
                });

                token = token_tmp;
                token_data = token_data_tmp;

                let Some(ws_tmp) = ws.send(
                    token.to_str().unwrap()
                ) else { return };
                ws = ws_tmp;
            }
            Err(e) => {
                error!(target: "login", "{e:?}");
                ws.close_frame(WebSocketCode::InternalError, "");
                return;
            }
        };

        logged_in(globals, ws, token, token_data).await;
    })
}

pub(crate) async fn quick_login(
    token: VerifiedToken<LoginTokenGranter>,
    State(globals): State<GlobalState>,
    ws: WebSocketUpgrade,
) -> Response {
    let user_profile = match globals
        .db
        .get_user_profile_by_email(token.item.email.clone())
        .await
    {
        Ok(Some(ref x)) => serde_json::to_string(x).expect("Correct serialization of UserProfile"),
        Ok(None) => {
            return Response::builder()
                .status(StatusCode::BAD_REQUEST)
                .body(BoxBody::default())
                .expect("Response to be valid")
        }
        Err(e) => {
            error!(target: "quick_login", "{e:?}");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(BoxBody::default())
                .expect("Response to be valid");
        }
    };

    ws.on_upgrade(|ws| async move {
        let ws = ManagedWebSocket::new(ws, WS_PING_DELAY);
        let Some(ws) = ws.send(user_profile) else { return };
        logged_in(globals, ws, token.token, token.item).await;
    })
}

// #[derive(Deserialize, serde::Serialize)]
#[derive(Deserialize)]
struct ScoreUpdateRequest<'a> {
    difficulty: &'a str,
    score: u16,
}

// #[derive(Deserialize, serde::Serialize)]
#[derive(Deserialize)]
enum MessageSet<'a> {
    #[serde(borrow)]
    ScoreUpdateRequest(ScoreUpdateRequest<'a>),
    Logout,
}

async fn logged_in(
    GlobalState {
        leaderboard,
        login_tokens,
        ..
    }: GlobalState,
    mut ws: ManagedWebSocket,
    token: Arc<HeaderValue>,
    token_data: Arc<LoginTokenData>,
) {
    let email = &token_data.email;
    let username = &token_data.username;

    macro_rules! recv {
        () => {{
            let Some((tmp_ws, msg)) = ws.recv().await else { break };
            ws = tmp_ws;
            msg
        }};
    }
    macro_rules! send {
        ($msg: expr) => {{
            let Some(tmp_ws) = ws.send($msg) else { break };
            ws = tmp_ws;
        }};
    }
    loop {
        let Message::Text(msg) = recv!() else {
            send!("Only text");
            continue;
        };
        let Ok(msg) = serde_json::from_str::<MessageSet>(&msg) else {
            send!("Bad Request");
            continue;
        };
        match msg {
            MessageSet::ScoreUpdateRequest(req) => {
                let res = match req.difficulty {
                    "easy" => {
                        leaderboard
                            .add_easy_entry(
                                email.clone(),
                                LeaderboardEntry {
                                    score: req.score,
                                    username: username.clone(),
                                },
                            )
                            .await
                    }
                    "normal" => {
                        leaderboard
                            .add_normal_entry(
                                email.clone(),
                                LeaderboardEntry {
                                    score: req.score,
                                    username: username.clone(),
                                },
                            )
                            .await
                    }
                    "expert" => {
                        leaderboard
                            .add_expert_entry(
                                email.clone(),
                                LeaderboardEntry {
                                    score: req.score,
                                    username: username.clone(),
                                },
                            )
                            .await
                    }
                    _ => {
                        send!("Not a valid difficulty");
                        continue;
                    }
                };

                if let Err(_e) = res {
                    send!("Internal Error");
                    continue;
                }

                send!("Success");
            }
            MessageSet::Logout => {
                login_tokens.revoke_token(token.deref());
                ws.close_frame(WebSocketCode::Ok, "Logout Successful");
                return;
            }
        }
    }
}
