use std::sync::Arc;

use axum::{
    async_trait,
    extract::{ws::Message, FromRequest, FromRequestParts},
    http::Request,
};
use mangle_api_core::{
    auth::token::{TokenVerificationError, VerifiedToken},
    log::{error, warn},
    neo_api::{APIMessage, ConnectionLock},
    serde_json,
    ws::{ManagedWebSocket, WebSocketCode},
};
use rustrict::CensorStr;
use serde::Deserialize;
use tokio::select;

use crate::{
    db::UserProfile,
    leaderboard::{LeaderboardEntry, LeaderboardUpdate},
    GlobalState, LoginTokenConfig, LoginTokenData,
};
use std::ops::Deref;
pub struct FirstConnectionState {
    globals: GlobalState,
    login_token: Option<VerifiedToken<LoginTokenConfig>>,
}

struct LoggedIn {
    login_token: VerifiedToken<LoginTokenConfig>,
    _conn_lock: ConnectionLock<Arc<String>>,
}

pub struct SessionState {
    globals: GlobalState,
    logged_in: Option<LoggedIn>,
}

#[async_trait]
impl<B> FromRequest<GlobalState, B> for FirstConnectionState
where
    B: Send + Sync + 'static,
{
    type Rejection = TokenVerificationError;

    async fn from_request(req: Request<B>, state: &GlobalState) -> Result<Self, Self::Rejection> {
        let (mut parts, _) = req.into_parts();
        let login_token =
            match VerifiedToken::<LoginTokenConfig>::from_request_parts(&mut parts, state).await {
                Ok(token) => Some(token),
                Err(TokenVerificationError::MissingToken) => None,
                Err(e) => return Err(e),
            };

        Ok(Self {
            globals: state.clone(),
            login_token,
        })
    }
}

#[derive(Deserialize)]
enum WSAPIMessageImpl<'a> {
    ScoreUpdateRequest {
        #[serde(borrow)]
        difficulty: &'a str,
        score: u16,
    },
    Logout,
    GetLeaderboard {
        #[serde(borrow)]
        difficulty: &'a str,
    },
    Login,
}

pub struct WSAPIMessage {
    _data: Box<str>,
    msg: WSAPIMessageImpl<'static>,
}

impl TryFrom<String> for WSAPIMessage {
    type Error = String;

    fn try_from(mut value: String) -> Result<Self, Self::Error> {
        value.shrink_to_fit();
        let data_ref = value.leak();
        let data = unsafe { Box::from_raw(data_ref) };

        Ok(Self {
            _data: data,
            msg: serde_json::from_str(data_ref).map_err(|_| "Bad Request")?,
        })
    }
}

#[async_trait]
impl APIMessage for WSAPIMessage {
    type FirstConnectionState = FirstConnectionState;
    type SessionState = SessionState;

    async fn on_connection(
        session_state: FirstConnectionState,
        mut ws: ManagedWebSocket,
    ) -> Option<(ManagedWebSocket, SessionState)> {
        let logged_in = if let Some(login_token) = session_state.login_token {
            let Ok(conn_lock) = session_state
                .globals
                .api_conn_manager
                .get_connection_lock(
                    Arc::new(login_token
                        .item
                        .email
                        .clone()))
                else {
                    ws.close_frame(WebSocketCode::BadPayload, "Already Connected");
                    return None
                };

            let profile = match session_state
                .globals
                .db
                .get_user_profile_by_email(&login_token.item.email)
                .await
            {
                Ok(Some(profile)) => profile,
                Ok(None) => {
                    warn!(
                        "Got valid session token without associated account: {}",
                        login_token.item.email
                    );
                    ws.close_frame(WebSocketCode::BadPayload, "User does not exist");
                    return None;
                }
                Err(e) => {
                    error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", login_token.item.email);
                    ws.close_frame(WebSocketCode::InternalError, "");
                    return None;
                }
            };

            ws = ws.send(serde_json::to_string(&profile).unwrap())?;

            Some(LoggedIn {
                _conn_lock: conn_lock,
                login_token,
            })
        } else {
            None
        };

        Some((
            ws,
            SessionState {
                globals: session_state.globals,
                logged_in,
            },
        ))
    }

    async fn route(
        self,
        session_state: &mut Self::SessionState,
        ws: ManagedWebSocket,
    ) -> Option<ManagedWebSocket> {
        let leaderboard = &session_state.globals.leaderboard;

        macro_rules! check_login {
            () => {{
                let Some(LoggedIn { login_token, .. }) = &session_state.logged_in else {
                                                        return ws.send("Not logged in")
                                                    };
                login_token
            }};
        }

        match self.msg {
            WSAPIMessageImpl::ScoreUpdateRequest { difficulty, score } => {
                let login_token = check_login!();
                let email = &login_token.item.email;
                let username = &login_token.item.username;

                let res = match difficulty {
                    "easy" => {
                        leaderboard
                            .add_easy_entry(
                                email.clone(),
                                LeaderboardEntry {
                                    score,
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
                                    score,
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
                                    score,
                                    username: username.clone(),
                                },
                            )
                            .await
                    }
                    _ => return ws.send("Not a valid difficulty"),
                };

                if let Err(_e) = res {
                    return ws.send("Internal Error");
                }

                ws.send("Success!")
            }
            WSAPIMessageImpl::Logout => {
                let login_token = check_login!();
                session_state
                    .globals
                    .login_tokens
                    .revoke_token(&login_token.token);
                ws.close_frame(WebSocketCode::Ok, "Successfully logged out");
                None
            }
            WSAPIMessageImpl::GetLeaderboard { difficulty } => {
                let leaderboard = &session_state.globals.leaderboard;

                macro_rules! leaderboard {
                    ($first_fn: ident, $second_fn: ident) => {{
                        let mut opt_ws = ws.send(serde_json::to_string(
                            &leaderboard.$first_fn()
                        ).unwrap());

                        loop {
                            select! {
                                opt = ManagedWebSocket::option_recv(&mut opt_ws) => {
                                    opt.as_ref()?;  // Return if None
                                    break opt_ws.unwrap().send("Unsubscribed")
                                }
                                opt = leaderboard.$second_fn() => {
                                    if let Some(LeaderboardUpdate{ leaderboard }) = opt {
                                        opt_ws = Some(opt_ws.unwrap().send(
                                            serde_json::to_string(leaderboard.deref())
                                                .expect("leaderboard to serialize")
                                        )?);
                                    } else {
                                        opt_ws.unwrap().close_frame(WebSocketCode::InternalError, "Leaderboard Closed");
                                        break None
                                    }
                                }
                            }
                        }
                    }};
                }

                match difficulty {
                    "easy" => leaderboard!(get_easy_leaderboard, wait_for_easy_update),
                    "normal" => leaderboard!(get_normal_leaderboard, wait_for_normal_update),
                    "expert" => leaderboard!(get_expert_leaderboard, wait_for_expert_update),
                    _ => ws.send("Not a valid difficulty"),
                }
            }
            WSAPIMessageImpl::Login => login(session_state, ws).await,
        }
    }
}

async fn login(
    session_state: &mut SessionState,
    mut ws: ManagedWebSocket,
) -> Option<ManagedWebSocket> {
    let globals = &session_state.globals;
    let db = &globals.db;
    let oidc = &globals.goidc.0;

    let (auth_url, fut) = oidc.initiate_auth(["openid", "email"]);

    ws = ws.send(auth_url.to_string())?;
    let mut opt_ws = Some(ws);

    let auth_option = select! {
        opt = fut => { opt }
        opt = ManagedWebSocket::option_recv(&mut opt_ws) => {
            opt.as_ref()?;  // Return if None
            return opt_ws.unwrap().send("Login Cancelled")
        }
    };
    ws = opt_ws.unwrap();

    let Some(data) = auth_option else {
        ws.close_frame(WebSocketCode::Ok, "Auth Failed");
        return None
    };
    let Some(email) = data.email else {
        ws.close_frame(WebSocketCode::BadPayload, "Auth Failed");
        return None
    };

    let Ok(conn_lock) = globals.api_conn_manager.get_connection_lock(
        Arc::new(email.clone())
    ) else {
        ws.close_frame(WebSocketCode::BadPayload, "Already Connected");
        return None
    };

    match db.get_user_profile_by_email(&email).await {
        Ok(Some(profile)) => {
            ws = ws.send(serde_json::to_string(&profile).unwrap())?;
            let login_token = globals.login_tokens.create_token(LoginTokenData {
                email,
                username: profile.username,
            });

            ws = ws.send(login_token.token.to_str().unwrap())?;

            session_state.logged_in = Some(LoggedIn {
                login_token,
                _conn_lock: conn_lock,
            });
        }
        Ok(None) => {
            ws = ws.send("Sign Up")?;
            let mut profile;

            loop {
                let (ws_tmp, msg) = ws.recv().await?;
                ws = ws_tmp;

                let Message::Text(msg) = msg else {
                    ws.close_frame(WebSocketCode::BadPayload, "Expected profile");
                    return None
                };

                let Ok(tmp_profile) = serde_json::from_str::<UserProfile>(&msg) else {
                    ws.close_frame(WebSocketCode::BadPayload, "");
                    return None
                };
                profile = tmp_profile;

                if profile.username.is_inappropriate() {
                    ws = ws.send("Inappropriate username")?;
                    continue;
                }

                match db.is_username_taken(&profile.username).await {
                    Ok(true) => {
                        ws = ws.send("Username already used")?;
                        continue;
                    }
                    Ok(false) => {}
                    Err(e) => {
                        error!(target: "login", "{:?}", e.context("checking if username is taken"));
                        ws.close_frame(WebSocketCode::InternalError, "");
                        return None;
                    }
                };

                break;
            }

            if let Err(e) = db
                .create_user_profile(
                    email.clone(),
                    profile.username.clone(),
                    profile.tournament_wins,
                )
                .await
            {
                error!(target: "login", "{:?}", e.context("creating user profile"));
                ws.close_frame(WebSocketCode::InternalError, "");
                return None;
            }

            let leaderboard = &globals.leaderboard;

            if let Err(e) = leaderboard
                .add_easy_entry(
                    email.clone(),
                    LeaderboardEntry {
                        score: profile.easy_highscore,
                        username: profile.username.clone(),
                    },
                )
                .await
            {
                let e = anyhow::Error::from(e);
                error!(target: "login", "{:?}", e.context(format!("adding easy entry for {email}")));
            }
            if let Err(e) = leaderboard
                .add_normal_entry(
                    email.clone(),
                    LeaderboardEntry {
                        score: profile.normal_highscore,
                        username: profile.username.clone(),
                    },
                )
                .await
            {
                let e = anyhow::Error::from(e);
                error!(target: "login", "{:?}", e.context(format!("adding normal entry for {email}")));
            }
            if let Err(e) = leaderboard
                .add_expert_entry(
                    email.clone(),
                    LeaderboardEntry {
                        score: profile.easy_highscore,
                        username: profile.username.clone(),
                    },
                )
                .await
            {
                let e = anyhow::Error::from(e);
                error!(target: "login", "{:?}", e.context(format!("adding easy entry for {email}")));
            }

            let login_token = globals.login_tokens.create_token(LoginTokenData {
                email,
                username: profile.username,
            });

            ws = ws.send(login_token.token.to_str().unwrap())?;

            session_state.logged_in = Some(LoggedIn {
                login_token,
                _conn_lock: conn_lock,
            });
        }
        Err(e) => {
            error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", email);
            ws.close_frame(WebSocketCode::InternalError, "");
            return None;
        }
    };

    Some(ws)
}
