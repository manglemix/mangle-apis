use axum::{extract::{State, WebSocketUpgrade, ws::Message}, response::{Response}, http::StatusCode, body::BoxBody};
use mangle_api_core::{auth::{token::{HeaderTokenGranter, VerifiedToken}}, ws::{ManagedWebSocket, WebSocketCode}, serde_json, log::error};
use rustrict::CensorStr;
use serde::Deserialize;
use tokio::select;

use crate::{leaderboard::{LeaderboardEntry}, db::{UserProfile}, LoginTokenGranter, WS_PING_DELAY, LoginTokenData, GlobalState};

#[axum::debug_handler(state = GlobalState)]
pub(crate) async fn login(
    State(globals): State<GlobalState>,
    ws: WebSocketUpgrade,
) -> Response {
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

        let username;

        match db.get_user_profile_by_email(&email).await {
            Ok(Some(profile)) => {
                let Some(ws_tmp) = ws.send(serde_json::to_string(&profile).unwrap()) else { return };
                username = profile.username;

                let Some(ws_tmp) = ws_tmp.send(
                    globals.login_tokens.create_token(LoginTokenData{
                        email: email.clone(),
                        username: username.clone()
                    }).to_str().unwrap()
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

                username = profile.username;

                if let Err(e) = db.create_user_profile(
                    email.clone(),
                    username.clone(),
                    profile.tournament_wins
                ).await {
                    error!(target: "login", "{:?}", e.context("creating user profile"));
                    ws.close_frame(WebSocketCode::InternalError, "");
                    return;
                }

                let leaderboard = &globals.leaderboard;

                if let Err(e) = leaderboard.add_easy_entry(email.clone(), LeaderboardEntry {
                    score: profile.easy_highscore,
                    username: username.clone(),
                }).await {
                    let e = anyhow::Error::from(e);
                    error!(target: "login", "{:?}", e.context(format!("adding easy entry for {email}")));
                }
                if let Err(e) = leaderboard.add_normal_entry(email.clone(), LeaderboardEntry {
                    score: profile.normal_highscore,
                    username: username.clone(),
                }).await {
                    let e = anyhow::Error::from(e);
                    error!(target: "login", "{:?}", e.context(format!("adding normal entry for {email}")));
                }
                if let Err(e) = leaderboard.add_expert_entry(email.clone(), LeaderboardEntry {
                    score: profile.easy_highscore,
                    username: username.clone(),
                }).await {
                    let e = anyhow::Error::from(e);
                    error!(target: "login", "{:?}", e.context(format!("adding easy entry for {email}")));
                }

                let Some(ws_tmp) = ws.send(
                    globals.login_tokens.create_token(LoginTokenData {
                        email: email.clone(),
                        username: username.clone()
                    }).to_str().unwrap()
                ) else { return };
                ws = ws_tmp;
            }
            Err(e) => {
                error!(target: "login", "{e:?}");
                ws.close_frame(WebSocketCode::InternalError, "");
                return;
            }
        };

        logged_in(globals, ws, email, username).await;
    })
}

pub(crate) async fn quick_login(
    token: VerifiedToken<LoginTokenGranter>,
    State(globals): State<GlobalState>,
    ws: WebSocketUpgrade
) -> Response {
    let user_profile = match globals.db.get_user_profile_by_email(token.item.email.clone()).await {
        Ok(Some(ref x)) => serde_json::to_string(x).expect("Correct serialization of UserProfile"),
        Ok(None) => return Response::builder()
            .status(StatusCode::BAD_REQUEST)
            .body(BoxBody::default())
            .expect("Response to be valid"),
        Err(e) => {
            error!(target: "quick_login", "{e:?}");
            return Response::builder()
                .status(StatusCode::INTERNAL_SERVER_ERROR)
                .body(BoxBody::default())
                .expect("Response to be valid")
        }
    };

    ws.on_upgrade(|ws| async move {
        let ws = ManagedWebSocket::new(ws, WS_PING_DELAY);
        let Some(ws) = ws.send(user_profile) else { return };
        logged_in(globals, ws, token.item.email.clone(), token.item.username.clone()).await;
    })
}


#[derive(Deserialize)]
struct ScoreUpdateRequest<'a> {
    difficulty: &'a str,
    score: u16
}


#[derive(Deserialize)]
enum MessageSet<'a> {
    #[serde(borrow)]
    ScoreUpdateRequest(ScoreUpdateRequest<'a>)
}


async fn logged_in(
    GlobalState { leaderboard, .. }: GlobalState,
    mut ws: ManagedWebSocket,
    email: String,
    username: String
) {
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
                    "easy" => leaderboard.add_easy_entry(
                        email.clone(),
                        LeaderboardEntry {
                            score: req.score,
                            username: username.clone()
                        }
                    ).await,
                    "normal" => leaderboard.add_normal_entry(
                        email.clone(),
                        LeaderboardEntry {
                            score: req.score,
                            username: username.clone()
                        }
                    ).await,
                    "expert" => leaderboard.add_expert_entry(
                        email.clone(),
                        LeaderboardEntry {
                            score: req.score,
                            username: username.clone()
                        }
                    ).await,
                    _ => {
                        send!("Not a valid difficulty");
                        continue
                    }
                };

                if let Err(_e) = res {
                    send!("Internal Error");
                    continue
                }

                send!("Success");
            }
        }
    }
}