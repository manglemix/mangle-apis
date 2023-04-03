use std::{
    marker::PhantomPinned,
    mem::{take, transmute},
    pin::Pin,
    sync::Arc,
    time::Instant,
};

use axum::{
    async_trait,
    extract::{ws::Message, FromRef, FromRequest, FromRequestParts},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use mangle_api_core::{
    auth::token::{TokenVerificationError, VerifiedToken},
    log::{error, warn},
    neo_api::{APIConnectionManager, APIMessage, ConnectionLock},
    serde_json,
    webrtc::{ICECandidate, JoinSessionError, SDPAnswer, SDPOffer, SDPOfferStream, ConnectionReceiver},
    ws::{ManagedWebSocket, WebSocketCode},
};
use rustrict::CensorStr;
use serde::{Deserialize, Serialize};
use tokio::select;

use crate::{
    db::UserProfile, leaderboard::LeaderboardEntry, multiplayer::RoomCode,
    tournament::TournamentData, GlobalState, LoginTokenConfig, LoginTokenData,
};
use std::ops::Deref;
pub struct FirstConnectionState {
    globals: GlobalState,
    logged_in: Option<LoggedIn>,
}

struct LoggedIn {
    login_token: VerifiedToken<LoginTokenConfig>,
    _conn_lock: ConnectionLock<Arc<String>>,
}


async fn webrtc_handle(mut ws: ManagedWebSocket, handle: &mut ConnectionReceiver) -> Option<ManagedWebSocket> {
    let mut opt_ws = Some(ws);

    loop {
        select! {
            opt = handle.wait_for_conn() => {
                ws = opt_ws.unwrap();
                let Some(SDPOfferStream { sdp_offer, answer_stream }) = opt else {
                    return ws.send("Room closed")
                };
                ws = ws.send(sdp_offer.0)?;

                let (tmp_ws, msg) = ws.recv().await?;
                ws = tmp_ws;

                let Message::Text(msg) = msg else { return ws.send("Bad Message") };
                if msg == "Cancel" {
                    return ws.send("Unhosted");
                }
                let Ok(msg) = WSAPIMessage::try_from(msg) else {
                    return ws.send("Bad Message")
                };
                let WSAPIMessageImpl::SDPAnswer { sdp_answer, ice_candidate } = msg.msg else {
                    return ws.send("Bad Message")
                };
                let ice_recv = answer_stream.send_answer(sdp_answer.into(), ice_candidate.into());
                let ice = ice_recv.get_ice().await;

                ws = ws.send(ice.0)?;

                opt_ws = Some(ws);
            }
            opt = ManagedWebSocket::option_recv(&mut opt_ws) => {
                opt.as_ref()?;  // Return if None
                break opt_ws.unwrap().send("Unhosted")
            }
        }
    }
}


pub struct SessionState {
    globals: GlobalState,
    logged_in: Option<LoggedIn>,
    last_leaderboard_retrieval: Option<Instant>,
}

#[async_trait]
impl<B> FromRequest<GlobalState, B> for FirstConnectionState
where
    B: Send + Sync + 'static,
{
    type Rejection = Response;

    async fn from_request(req: Request<B>, state: &GlobalState) -> Result<Self, Self::Rejection> {
        let (mut parts, _) = req.into_parts();
        let logged_in =
            match VerifiedToken::<LoginTokenConfig>::from_request_parts(&mut parts, state).await {
                Ok(login_token) => Some(LoggedIn {
                    _conn_lock: {
                        let conn_manager = APIConnectionManager::from_ref(state);
                        conn_manager
                            .get_connection_lock(Arc::new(login_token.item.email.clone()))
                            .map_err(|_| {
                                (StatusCode::CONFLICT, "Already Connected").into_response()
                            })
                    }?,
                    login_token,
                }),
                Err(TokenVerificationError::MissingToken) => None,
                Err(e) => return Err(e.into_response()),
            };

        Ok(Self {
            globals: state.clone(),
            logged_in,
        })
    }
}

fn default_lobby_size() -> usize {
    4
}

#[derive(Deserialize)]
enum WSAPIMessageImpl<'a> {
    ScoreUpdateRequest {
        #[serde(borrow)]
        difficulty: &'a str,
        score: u16,
    },
    Logout,
    GetLeaderboard,
    Login,
    GetTournament,
    WinTournament,
    HostSession {
        #[serde(default = "default_lobby_size")]
        max_size: usize,
    },
    StartJoinSession(
        // session code
        u16,
    ),
    JoinSessionSDPOffers(Vec<String>),
    JoinSessionICE {
        index: usize,
        ice: String,
    },
    SDPAnswer {
        sdp_answer: String,
        ice_candidate: String,
    },
}

pub struct WSAPIMessage {
    // str will not move, thus making this self referential struct safe
    _data: Pin<Box<(PhantomPinned, str)>>,
    msg: WSAPIMessageImpl<'static>,
}

impl TryFrom<String> for WSAPIMessage {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let data = value.into_boxed_str();
        let data: Pin<Box<(PhantomPinned, str)>> = unsafe { transmute(data) };

        Ok(Self {
            // SAFETY: Ref to data will live for as long as this struct exists,
            // and the data will not be modified
            // This transmute makes the lifetimes work
            msg: serde_json::from_str(unsafe { transmute(data.deref()) })
                .map_err(|_| "Bad Request")?,
            _data: data,
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
        if let Some(logged_in) = &session_state.logged_in {
            let profile = match session_state
                .globals
                .db
                .get_user_profile_by_email(&logged_in.login_token.item.email)
                .await
            {
                Ok(Some(profile)) => profile,
                Ok(None) => {
                    warn!(
                        "Got valid session token without associated account: {}",
                        logged_in.login_token.item.email
                    );
                    ws.close_frame(WebSocketCode::BadPayload, "User does not exist");
                    return None;
                }
                Err(e) => {
                    error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", logged_in.login_token.item.email);
                    ws.close_frame(WebSocketCode::InternalError, "");
                    return None;
                }
            };

            ws = ws.send(serde_json::to_string(&profile).unwrap())?;
        }

        Some((
            ws,
            SessionState {
                globals: session_state.globals,
                logged_in: session_state.logged_in,
                last_leaderboard_retrieval: None,
            },
        ))
    }

    async fn route(
        self,
        session_state: &mut Self::SessionState,
        mut ws: ManagedWebSocket,
    ) -> Option<ManagedWebSocket> {
        let leaderboard = &session_state.globals.leaderboard;
        let multiplayer = &session_state.globals.multiplayer;

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

                ws.send("Success")
            }
            WSAPIMessageImpl::Logout => {
                let login_token = check_login!();
                session_state
                    .globals
                    .login_tokens
                    .revoke_token(&login_token.token);
                session_state.logged_in = None;
                ws.send("Success")
            }
            WSAPIMessageImpl::GetLeaderboard => {
                let leaderboard = &session_state.globals.leaderboard;

                let mut opt_ws = if let Some(inst) = session_state.last_leaderboard_retrieval {
                    if let Some(leaderboard) = leaderboard.get_leaderboard_since(inst) {
                        ws.send(serde_json::to_string(&leaderboard).unwrap())
                    } else {
                        Some(ws)
                    }
                } else {
                    ws.send(serde_json::to_string(&leaderboard.get_leaderboard()).unwrap())
                };

                opt_ws = loop {
                    select! {
                        opt = ManagedWebSocket::option_recv(&mut opt_ws) => {
                            opt.as_ref()?;  // Return if None
                            break opt_ws.unwrap().send("Unsubscribed")
                        }
                        opt = leaderboard.wait_for_update() => {
                            if let Some(update) = opt {
                                opt_ws = Some(opt_ws.unwrap().send(
                                    serde_json::to_string(update.deref()).unwrap()
                                )?);

                            } else {
                                opt_ws.unwrap().close_frame(WebSocketCode::InternalError, "Leaderboard Closed");
                                break None
                            }
                        }
                    }
                };
                session_state.last_leaderboard_retrieval = Some(Instant::now());
                opt_ws
            }
            WSAPIMessageImpl::Login => {
                if session_state.logged_in.is_some() {
                    ws.send("Already Logged In")
                } else {
                    login(session_state, ws).await
                }
            }
            WSAPIMessageImpl::GetTournament => {
                match session_state.globals.tournament.get_tournament_week() {
                    Some(data) => ws.send(serde_json::to_string(&data).unwrap()),
                    None => ws.send("Internal Error"),
                }
            }
            WSAPIMessageImpl::WinTournament => {
                let login_token = check_login!();

                let Some(TournamentData { week, .. }) = session_state.globals.tournament.get_tournament_week() else {
                    return ws.send("Internal Error")
                };

                if let Err(e) = session_state
                    .globals
                    .db
                    .win_tournament(week, login_token.item.email.to_string())
                    .await
                {
                    error!(target: "tournament", "Faced the following error while winning tournament for {}: {e:?}", login_token.item.email);
                    ws.send("Internal Error")
                } else {
                    ws.send("Success")
                }
            }
            WSAPIMessageImpl::HostSession { max_size } => {
                let (mut handle, code) = multiplayer.host_session_random_id(max_size);
                ws = ws.send(code.to_string())?;

                webrtc_handle(ws, &mut handle).await
            }
            WSAPIMessageImpl::StartJoinSession(code) => {
                let Ok(code) = RoomCode::try_from(code) else {
                    return ws.send("Bad code")
                };
                let mut offer_sender = match multiplayer.join_session(&code) {
                    Ok(x) => x,
                    Err(JoinSessionError::Full) => return ws.send("Room Full"),
                    Err(JoinSessionError::NotFound) => return ws.send("Not Found"),
                };

                let member_count = offer_sender.get_member_count();
                ws = ws.send(member_count.to_string())?;

                let (mut handle, mut answer_streams) = loop {
                    let (tmp_ws, msg) = ws.recv().await?;
                    ws = tmp_ws;

                    let Message::Text(msg) = msg else { return ws.send("Bad Message") };
                    if msg == "Cancel" {
                        return ws.send("Cancelled");
                    }
                    let Ok(msg) = WSAPIMessage::try_from(msg) else {
                        return ws.send("Bad Message")
                    };
                    let WSAPIMessageImpl::JoinSessionSDPOffers(offers) = msg.msg else {
                        return ws.send("Bad Message")
                    };
                    match offer_sender
                        .send_sdp_offers(offers.into_iter().map(SDPOffer::from).collect())
                        .await
                    {
                        Ok(x) => break x,
                        Err((tmp_offer_sender, _)) => {
                            offer_sender = tmp_offer_sender;
                            continue;
                        }
                    }
                };

                let mut ice_senders = Vec::with_capacity(member_count);

                for _ in 0..member_count {
                    ice_senders.push(None);
                }

                while let Some((index, SDPAnswer(sdp_answer), ICECandidate(ice), ice_sender)) =
                    answer_streams.wait_for_an_answer().await
                {
                    #[derive(Serialize)]
                    struct AnswerJSON {
                        index: usize,
                        sdp_answer: String,
                        ice: String,
                    }
                    *ice_senders.get_mut(index).unwrap() = Some(ice_sender);
                    ws = ws.send(
                        serde_json::to_string(&AnswerJSON {
                            index,
                            sdp_answer,
                            ice,
                        })
                        .unwrap(),
                    )?;
                }

                let mut remaining = member_count;
                while remaining > 0 {
                    let (tmp_ws, msg) = ws.recv().await?;
                    ws = tmp_ws;

                    let Message::Text(msg) = msg else {
                        ws = ws.send("Bad Message")?;
                        continue
                    };
                    if msg == "Cancel" {
                        return ws.send("Cancelled");
                    }
                    let Ok(msg) = WSAPIMessage::try_from(msg) else {
                        ws = ws.send("Bad Message")?;
                        continue
                    };
                    let Ok(msg) = WSAPIMessage::try_from(msg) else {
                        ws = ws.send("Bad Message")?;
                        continue
                    };
                    let WSAPIMessageImpl::JoinSessionICE{ index, ice } = msg.msg else {
                        ws = ws.send("Bad Message")?;
                        continue
                    };
                    let Some(ice_sender) = take(ice_senders.get_mut(index).unwrap()) else {
                        ws = ws.send("Already sent")?;
                        continue
                    };
                    remaining -= 1;
                    ice_sender.send(ice.into()).await;
                }
                
                webrtc_handle(ws, &mut handle).await
            }
            WSAPIMessageImpl::JoinSessionSDPOffers(_)
            | WSAPIMessageImpl::JoinSessionICE { .. }
            | WSAPIMessageImpl::SDPAnswer { .. } => ws.send("Must be in session"),
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
        return ws.send("Auth Failed")
    };
    let Some(email) = data.email else {
        return ws.send("Auth Failed")
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
                error!(target: "login", "{:?}", e.context(format!("adding expert entry for {email}")));
            }

            ws = ws.send("Success")?;

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
