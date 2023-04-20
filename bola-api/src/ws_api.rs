use std::{
    sync::Arc,
    time::Instant,
};

use axum::{
    async_trait,
    extract::{FromRef, FromRequest, FromRequestParts},
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
};
use log::{error, warn};
use mangle_api_core::{
    self,
    auth::{token::{TokenVerificationError, VerifiedToken}, openid::OIDC},
    serde_json,
    webrtc::{
        ConnectionReceiver, ICECandidate, JoinSessionError, SDPAnswer, SDPOffer, SDPOfferStream,
    },
    ws::{ManagedWebSocket, WebSocketCode, WsError}, neo_api::NeoApiConfig,
};
use messagist::{ExclusiveMessageHandler, MessageStream};
use rustrict::CensorStr;
use serde::{Deserialize};
use tokio::select;

use crate::{
    db::{UserProfile, DB}, leaderboard::{LeaderboardEntry, Leaderboard, self}, multiplayer::RoomCode,
    tournament::TournamentData, GlobalState, LoginTokenConfig, LoginTokenData, LoginTokenGranter,
};

// async fn handle_webrtc(
//     mut ws: ManagedWebSocket,
//     handle: &mut ConnectionReceiver,
// ) -> Option<ManagedWebSocket> {
//     let mut opt_ws = Some(ws);

//     loop {
//         select! {
//             opt = handle.wait_for_conn() => {
//                 ws = opt_ws.unwrap();
//                 let Some(SDPOfferStream { sdp_offer, answer_stream }) = opt else {
//                     return ws.send("Room closed")
//                 };
//                 ws = ws.send(sdp_offer.0)?;

//                 let (tmp_ws, msg) = ws.recv().await?;
//                 ws = tmp_ws;

//                 let Message::Text(msg) = msg else { return ws.send("Bad Message") };
//                 if msg == "Cancel" {
//                     return ws.send("Unhosted");
//                 }
//                 let Ok(msg) = WSAPIMessage::try_from(msg) else {
//                     return ws.send("Bad Message")
//                 };
//                 let WSAPIMessage::SDPAnswer { sdp_answer, ice_candidate } = msg.msg else {
//                     return ws.send("Bad Message")
//                 };
//                 let ice_recv = answer_stream.send_answer(sdp_answer.into(), ice_candidate.into());
//                 let ice = ice_recv.get_ice().await;

//                 ws = ws.send(ice.0)?;

//                 opt_ws = Some(ws);
//             }
//             opt = ManagedWebSocket::option_recv(&mut opt_ws) => {
//                 opt.as_ref()?;  // Return if None
//                 break opt_ws.unwrap().send("Unhosted")
//             }
//         }
//     }
// }

pub struct SessionState {
    login_token: Option<VerifiedToken<LoginTokenConfig>>,
    last_leaderboard_retrieval: Option<Instant>,
}

#[async_trait]
impl<B> FromRequest<GlobalState, B> for SessionState
where
    B: Send + Sync + 'static,
{
    type Rejection = Response;

    async fn from_request(req: Request<B>, state: &GlobalState) -> Result<Self, Self::Rejection> {
        let (mut parts, _) = req.into_parts();
        let login_token =
            match VerifiedToken::<LoginTokenConfig>::from_request_parts(&mut parts, state).await {
                Ok(x) => {
                    let api = NeoApiConfig::from_ref(state);
                    let api = api.get_handler();

                    if api.inner.connections.contains(&x.item.email) {
                        return Err((StatusCode::CONFLICT, "Already Connected").into_response())
                    }
                    api.inner.connections.insert(x.item.email.clone());
                    Some(x)
                }
                Err(TokenVerificationError::MissingToken) => None,
                Err(e) => return Err(e.into_response()),
            };

        Ok(Self {
            login_token,
            last_leaderboard_retrieval: None
        })
    }
}

fn default_lobby_size() -> usize {
    4
}

#[derive(Deserialize)]
enum WSAPIMessage {
    ScoreUpdateRequest {
        difficulty: String,
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

pub struct WsApiHandlerImpl {
    connections: dashmap::DashSet<String>,
    leaderboard: Leaderboard,
    db: DB,
    oidc: OIDC,
    login_tokens: LoginTokenGranter
}

#[derive(Clone)]
pub struct WsApiHandler {
    inner: Arc<WsApiHandlerImpl>,
}

#[async_trait]
impl ExclusiveMessageHandler for WsApiHandler {
    type SessionState = SessionState;

    // async fn on_connection(
    //     session_state: FirstConnectionState,
    //     mut ws: ManagedWebSocket,
    // ) -> Option<(ManagedWebSocket, SessionState)> {
    //     if let Some(logged_in) = &session_state.logged_in {
    //         let profile = match session_state
    //             .globals
    //             .db
    //             .get_user_profile_by_email(&logged_in.login_token.item.email)
    //             .await
    //         {
    //             Ok(Some(profile)) => profile,
    //             Ok(None) => {
    //                 warn!(
    //                     "Got valid session token without associated account: {}",
    //                     logged_in.login_token.item.email
    //                 );
    //                 ws.close_frame(WebSocketCode::BadPayload, "User does not exist");
    //                 return None;
    //             }
    //             Err(e) => {
    //                 error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", logged_in.login_token.item.email);
    //                 ws.close_frame(WebSocketCode::InternalError, "");
    //                 return None;
    //             }
    //         };

    //         ws = ws.send(serde_json::to_string(&profile).unwrap())?;
    //     }

    //     Some((
    //         ws,
    //         SessionState {
    //             globals: session_state.globals,
    //             logged_in: session_state.logged_in,
    //             last_leaderboard_retrieval: None,
    //         },
    //     ))
    // }

    // async fn route(
    //     self,
    //     session_state: &mut Self::SessionState,
    //     mut ws: ManagedWebSocket,
    // ) -> Option<ManagedWebSocket> {
    //     let leaderboard = &session_state.globals.leaderboard;
    //     let multiplayer = &session_state.globals.multiplayer;

    //     macro_rules! check_login {
    //         () => {{
    //             let Some(LoggedIn { login_token, .. }) = &session_state.logged_in else {
    //                                                                 return ws.send("Not logged in")
    //                                                             };
    //             login_token
    //         }};
    //     }

    //     match self.msg {
    //         WSAPIMessageImpl::ScoreUpdateRequest { difficulty, score } => {
    //             let login_token = check_login!();
    //             let email = &login_token.item.email;
    //             let username = &login_token.item.username;

    //             let res = match difficulty {
    //                 "easy" => {
    //                     leaderboard
    //                         .add_easy_entry(
    //                             email.clone(),
    //                             LeaderboardEntry {
    //                                 score,
    //                                 username: username.clone(),
    //                             },
    //                         )
    //                         .await
    //                 }
    //                 "normal" => {
    //                     leaderboard
    //                         .add_normal_entry(
    //                             email.clone(),
    //                             LeaderboardEntry {
    //                                 score,
    //                                 username: username.clone(),
    //                             },
    //                         )
    //                         .await
    //                 }
    //                 "expert" => {
    //                     leaderboard
    //                         .add_expert_entry(
    //                             email.clone(),
    //                             LeaderboardEntry {
    //                                 score,
    //                                 username: username.clone(),
    //                             },
    //                         )
    //                         .await
    //                 }
    //                 _ => return ws.send("Not a valid difficulty"),
    //             };

    //             if let Err(_e) = res {
    //                 return ws.send("Internal Error");
    //             }

    //             ws.send("Success")
    //         }
    //         WSAPIMessageImpl::Logout => {
    //             let login_token = check_login!();
    //             session_state
    //                 .globals
    //                 .login_tokens
    //                 .revoke_token(&login_token.token);
    //             session_state.logged_in = None;
    //             ws.send("Success")
    //         }
    //         WSAPIMessageImpl::GetLeaderboard => {
    //             let leaderboard = &session_state.globals.leaderboard;

    //             let mut opt_ws = if let Some(inst) = session_state.last_leaderboard_retrieval {
    //                 if let Some(leaderboard) = leaderboard.get_leaderboard_since(inst) {
    //                     ws.send(serde_json::to_string(&leaderboard).unwrap())
    //                 } else {
    //                     Some(ws)
    //                 }
    //             } else {
    //                 ws.send(serde_json::to_string(&leaderboard.get_leaderboard()).unwrap())
    //             };

    //             opt_ws = loop {
    //                 select! {
    //                     opt = ManagedWebSocket::option_recv(&mut opt_ws) => {
    //                         opt.as_ref()?;  // Return if None
    //                         break opt_ws.unwrap().send("Unsubscribed")
    //                     }
    //                     opt = leaderboard.wait_for_update() => {
    //                         if let Some(update) = opt {
    //                             opt_ws = Some(opt_ws.unwrap().send(
    //                                 serde_json::to_string(update.deref()).unwrap()
    //                             )?);

    //                         } else {
    //                             opt_ws.unwrap().close_frame(WebSocketCode::InternalError, "Leaderboard Closed");
    //                             break None
    //                         }
    //                     }
    //                 }
    //             };
    //             session_state.last_leaderboard_retrieval = Some(Instant::now());
    //             opt_ws
    //         }
    //         WSAPIMessageImpl::Login => {
    //             if session_state.logged_in.is_some() {
    //                 ws.send("Already Logged In")
    //             } else {
    //                 login(session_state, ws).await
    //             }
    //         }
    //         WSAPIMessageImpl::GetTournament => {
    //             match session_state.globals.tournament.get_tournament_week() {
    //                 Some(data) => ws.send(serde_json::to_string(&data).unwrap()),
    //                 None => ws.send("Internal Error"),
    //             }
    //         }
    //         WSAPIMessageImpl::WinTournament => {
    //             let login_token = check_login!();

    //             let Some(TournamentData { week, .. }) = session_state.globals.tournament.get_tournament_week() else {
    //                 return ws.send("Internal Error")
    //             };

    //             if let Err(e) = session_state
    //                 .globals
    //                 .db
    //                 .win_tournament(week, login_token.item.email.to_string())
    //                 .await
    //             {
    //                 error!(target: "tournament", "Faced the following error while winning tournament for {}: {e:?}", login_token.item.email);
    //                 ws.send("Internal Error")
    //             } else {
    //                 ws.send("Success")
    //             }
    //         }
    //         WSAPIMessageImpl::HostSession { max_size } => {
    //             let (mut handle, code) = multiplayer.host_session_random_id(max_size);
    //             ws = ws.send(code.to_string())?;

    //             handle_webrtc(ws, &mut handle).await
    //         }
    //         WSAPIMessageImpl::StartJoinSession(code) => {
    //             let Ok(code) = RoomCode::try_from(code) else {
    //                 return ws.send("Bad code")
    //             };
    //             let mut offer_sender = match multiplayer.join_session(&code) {
    //                 Ok(x) => x,
    //                 Err(JoinSessionError::Full) => return ws.send("Room Full"),
    //                 Err(JoinSessionError::NotFound) => return ws.send("Not Found"),
    //             };

    //             let member_count = offer_sender.get_member_count();
    //             ws = ws.send(member_count.to_string())?;

    //             let (mut handle, mut answer_streams) = loop {
    //                 let (tmp_ws, msg) = ws.recv().await?;
    //                 ws = tmp_ws;

    //                 let Message::Text(msg) = msg else { return ws.send("Bad Message") };
    //                 if msg == "Cancel" {
    //                     return ws.send("Cancelled");
    //                 }
    //                 let Ok(msg) = WSAPIMessage::try_from(msg) else {
    //                     return ws.send("Bad Message")
    //                 };
    //                 let WSAPIMessageImpl::JoinSessionSDPOffers(offers) = msg.msg else {
    //                     return ws.send("Bad Message")
    //                 };
    //                 match offer_sender
    //                     .send_sdp_offers(offers.into_iter().map(SDPOffer::from).collect())
    //                     .await
    //                 {
    //                     Ok(x) => break x,
    //                     Err((tmp_offer_sender, _)) => {
    //                         offer_sender = tmp_offer_sender;
    //                         continue;
    //                     }
    //                 }
    //             };

    //             let mut ice_senders = Vec::with_capacity(member_count);

    //             for _ in 0..member_count {
    //                 ice_senders.push(None);
    //             }

    //             while let Some((index, SDPAnswer(sdp_answer), ICECandidate(ice), ice_sender)) =
    //                 answer_streams.wait_for_an_answer().await
    //             {
    //                 #[derive(Serialize)]
    //                 struct AnswerJSON {
    //                     index: usize,
    //                     sdp_answer: String,
    //                     ice: String,
    //                 }
    //                 *ice_senders.get_mut(index).unwrap() = Some(ice_sender);
    //                 ws = ws.send(
    //                     serde_json::to_string(&AnswerJSON {
    //                         index,
    //                         sdp_answer,
    //                         ice,
    //                     })
    //                     .unwrap(),
    //                 )?;
    //             }

    //             let mut remaining = member_count;
    //             while remaining > 0 {
    //                 let (tmp_ws, msg) = ws.recv().await?;
    //                 ws = tmp_ws;

    //                 let Message::Text(msg) = msg else {
    //                     ws = ws.send("Bad Message")?;
    //                     continue
    //                 };
    //                 if msg == "Cancel" {
    //                     return ws.send("Cancelled");
    //                 }
    //                 let Ok(msg) = WSAPIMessage::try_from(msg) else {
    //                     ws = ws.send("Bad Message")?;
    //                     continue
    //                 };
    //                 let Ok(msg) = WSAPIMessage::try_from(msg) else {
    //                     ws = ws.send("Bad Message")?;
    //                     continue
    //                 };
    //                 let WSAPIMessageImpl::JoinSessionICE{ index, ice } = msg.msg else {
    //                     ws = ws.send("Bad Message")?;
    //                     continue
    //                 };
    //                 let Some(ice_sender) = take(ice_senders.get_mut(index).unwrap()) else {
    //                     ws = ws.send("Already sent")?;
    //                     continue
    //                 };
    //                 remaining -= 1;
    //                 ice_sender.send(ice.into()).await;
    //             }

    //             handle_webrtc(ws, &mut handle).await
    //         }
    //         WSAPIMessageImpl::JoinSessionSDPOffers(_)
    //         | WSAPIMessageImpl::JoinSessionICE { .. }
    //         | WSAPIMessageImpl::SDPAnswer { .. } => ws.send("Must be in session"),
    //     }
    // }

    async fn handle<S: MessageStream>(
        &mut self,
        mut stream: S,
        mut session_state: Self::SessionState,
    ) {
        macro_rules! send {
            ($msg: expr) => {
                if let Err(_) = stream.send_message($msg).await {
                    return;
                }
            };
        }
        loop {
            let Ok(msg) = stream.recv_message::<WSAPIMessage>().await else { break };

            if let Some(login_token) = &session_state.login_token {
                let leaderboard = &self.inner.leaderboard;

                match msg {
                    WSAPIMessage::ScoreUpdateRequest { difficulty, score } => {
                        let email = &login_token.item.email;
                        let username = &login_token.item.username;
        
                        let res = match difficulty.as_str() {
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
                            _ => {
                                send!("Not a valid difficulty");
                                return 
                            }
                        };
        
                        if let Err(_e) = res {
                            send!("Internal Error");
                        }
        
                        send!("Success");
                    }
                    WSAPIMessage::Login => {
                        send!("Already logged in");
                    }
                    _ => todo!()
                }
            } else {
                match msg {
                    WSAPIMessage::GetLeaderboard => { }
                    WSAPIMessage::GetTournament => { }
                    WSAPIMessage::Login => match self.login(&mut session_state, &mut stream).await {
                        Ok(StreamStatus::Closed) => break,
                        Ok(_) => { }
                        Err(_) => break
                    }

                    _ => send!("Must be logged in")
                }
            }
        }
    }
}


enum StreamStatus {
    Ok,
    Closed
}


impl WsApiHandler{
    pub(crate) fn new(leaderboard: Leaderboard, db: DB, oidc: OIDC, login_tokens: LoginTokenGranter) -> Self {
        Self {
            inner: Arc::new(WsApiHandlerImpl {
                connections: Default::default(),
                leaderboard,
                db,
                oidc,
                login_tokens
            })
        }
    }
    async fn login<S: MessageStream>(
        &mut self,
        session_state: &mut SessionState,
        stream: &mut S
    ) -> Result<StreamStatus, S::Error> {
        let db = &self.inner.db;
        let oidc = &self.inner.oidc;
        let leaderboard = &self.inner.leaderboard;
        let login_tokens = &self.inner.login_tokens;

        macro_rules! send {
            ($msg: expr) => {
                stream.send_message($msg).await?
            };
        }
        macro_rules! recv {
            () => {
                stream.recv_message().await?
            };
        }
        macro_rules! close {
            ($msg: expr) => {{
                send!($msg);
                return Ok(StreamStatus::Closed)
            }};
        }

        let (auth_url, fut) = oidc.initiate_auth(["openid", "email"]);

        send!(auth_url);

        let auth_option = select! {
            opt = fut => { opt }
            err = stream.wait_for_error() => {
                return Err(err)
            }
        };

        let Some(data) = auth_option else {
            send!("Auth Failed");
            return Ok(StreamStatus::Ok)
        };
        let Some(email) = data.email else {
            send!("Auth Failed");
            return Ok(StreamStatus::Ok)
        };

        if self.inner.connections.contains(&email) {
            send!("Already Connected");
        } else {
            self.inner.connections.insert(email.clone());
        }

        match db.get_user_profile_by_email(&email).await {
            Ok(Some(profile)) => {
                send!(&profile);
                let login_token = login_tokens.create_token(LoginTokenData {
                    email,
                    username: profile.username,
                });

                send!(login_token.token.to_str().unwrap());

                session_state.login_token = Some(login_token);
            }
            Ok(None) => {
                send!("Sign Up");
                let mut profile: UserProfile;

                loop {
                    profile = recv!();

                    if profile.username.is_inappropriate() {
                        send!("Inappropriate username");
                        continue;
                    }

                    match db.is_username_taken(&profile.username).await {
                        Ok(true) => {
                            send!("Username already used");
                            continue;
                        }
                        Ok(false) => {}
                        Err(e) => {
                            error!(target: "login", "{:?}", e.context("checking if username is taken"));
                            close!("Internal Error");
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
                    close!("Internal Error");
                }

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

                send!("Success");

                let login_token = login_tokens.create_token(LoginTokenData {
                    email,
                    username: profile.username,
                });

                send!(login_token.token.to_str().unwrap());

                session_state.login_token = Some(login_token);
            }
            Err(e) => {
                error!(target: "login", "Faced the following error while getting user profile for {}: {e:?}", email);
                close!("Internal Error");
            }
        };
        
        Ok(StreamStatus::Ok)
    }
}