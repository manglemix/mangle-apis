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
    db::UserProfile, leaderboard::LeaderboardEntry, GlobalState, LoginTokenConfig, LoginTokenData,
};

pub struct FirstConnectionState {
    globals: GlobalState,
    token: Option<VerifiedToken<LoginTokenConfig>>,
}

pub struct SessionState {
    globals: GlobalState,
    token: VerifiedToken<LoginTokenConfig>,
    _conn_lock: ConnectionLock<Arc<String>>,
}

#[async_trait]
impl<B> FromRequest<GlobalState, B> for FirstConnectionState
where
    B: Send + Sync + 'static,
{
    type Rejection = TokenVerificationError;

    async fn from_request(req: Request<B>, state: &GlobalState) -> Result<Self, Self::Rejection> {
        let (mut parts, _) = req.into_parts();
        let token =
            match VerifiedToken::<LoginTokenConfig>::from_request_parts(&mut parts, state).await {
                Ok(token) => Some(token),
                Err(TokenVerificationError::MissingToken) => None,
                Err(e) => return Err(e),
            };

        Ok(Self {
            globals: state.clone(),
            token,
        })
    }
}

#[derive(Deserialize)]
struct ScoreUpdateRequest<'a> {
    difficulty: &'a str,
    score: u16,
}

#[derive(Deserialize)]
enum WSAPIMessageImpl<'a> {
    #[serde(borrow)]
    ScoreUpdateRequest(ScoreUpdateRequest<'a>),
    Logout,
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
        if let Some(token) = session_state.token {
            let Ok(conn_lock) = session_state
                .globals
                .api_conn_manager
                .get_connection_lock(
                    Arc::new(token
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
                .get_user_profile_by_email(&token.item.email)
                .await
            {
                Ok(Some(profile)) => profile,
                Ok(None) => {
                    warn!(
                        "Got valid session token without associated account: {}",
                        token.item.email
                    );
                    ws.close_frame(WebSocketCode::BadPayload, "User does not exist");
                    return None;
                }
                Err(e) => {
                    error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", token.item.email);
                    ws.close_frame(WebSocketCode::InternalError, "");
                    return None;
                }
            };

            ws = ws.send(serde_json::to_string(&profile).unwrap())?;

            Some((
                ws,
                SessionState {
                    globals: session_state.globals,
                    token,
                    _conn_lock: conn_lock,
                },
            ))
        } else {
            login(session_state, ws).await
        }
    }

    async fn route(
        self,
        session_state: &mut Self::SessionState,
        ws: ManagedWebSocket,
    ) -> Option<ManagedWebSocket> {
        let leaderboard = &session_state.globals.leaderboard;
        let email = &session_state.token.item.email;
        let username = &session_state.token.item.username;

        match self.msg {
            WSAPIMessageImpl::ScoreUpdateRequest(req) => {
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
                    _ => return ws.send("Not a valid difficulty"),
                };

                if let Err(_e) = res {
                    return ws.send("Internal Error");
                }

                ws.send("Success!")
            }
            WSAPIMessageImpl::Logout => {
                session_state
                    .globals
                    .login_tokens
                    .revoke_token(&session_state.token.token);
                ws.close_frame(WebSocketCode::Ok, "Successfully logged out");
                None
            }
        }
    }
}

async fn login(
    session_state: FirstConnectionState,
    mut ws: ManagedWebSocket,
) -> Option<(ManagedWebSocket, SessionState)> {
    let globals = &session_state.globals;
    let db = &globals.db;
    let oidc = &globals.goidc.0;

    let (auth_url, fut) = oidc.initiate_auth(["openid", "email"]);

    ws = ws.send(auth_url.to_string())?;
    let mut ws = Some(ws);

    let opt = select! {
        opt = fut => { opt }
        () = ManagedWebSocket::loop_recv(&mut ws, None) => { return None }
    };
    let mut ws = ws.unwrap();

    let Some(data) = opt else {
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

    let new_session_state;

    match db.get_user_profile_by_email(&email).await {
        Ok(Some(profile)) => {
            ws = ws.send(serde_json::to_string(&profile).unwrap())?;
            let token = globals.login_tokens.create_token(LoginTokenData {
                email,
                username: profile.username,
            });

            ws = ws.send(token.token.to_str().unwrap())?;

            new_session_state = SessionState {
                globals: session_state.globals,
                token,
                _conn_lock: conn_lock,
            };
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

            let token = globals.login_tokens.create_token(LoginTokenData {
                email,
                username: profile.username,
            });

            ws = ws.send(token.token.to_str().unwrap())?;

            new_session_state = SessionState {
                globals: session_state.globals,
                token,
                _conn_lock: conn_lock,
            };
        }
        Err(e) => {
            error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", email);
            ws.close_frame(WebSocketCode::InternalError, "");
            return None;
        }
    };

    Some((ws, new_session_state))
}
