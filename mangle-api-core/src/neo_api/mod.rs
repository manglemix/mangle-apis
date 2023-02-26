use std::{sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{ws::Message, FromRef, FromRequest, WebSocketUpgrade, State},
    response::{Response},
    routing::MethodRouter,
};

use crate::ws::ManagedWebSocket;

#[async_trait]
pub trait APIMessage: Sized + Send + Sync + TryFrom<String, Error = String> + 'static {
    type FirstConnectionState: Send + Sync + 'static;
    type SessionState: Send + Sync + 'static;

    async fn on_connection(
        session_state: Self::FirstConnectionState,
        ws: ManagedWebSocket,
    ) -> Option<(ManagedWebSocket, Self::SessionState)>;
    async fn route(
        self,
        session_state: &mut Self::SessionState,
        ws: ManagedWebSocket,
    ) -> Option<ManagedWebSocket>;
}

struct APIConnectionManagerImpl {
    // connections: Mutex<HashSet<T>>,
    ping_delay: Duration,
}


// struct ClientConnection<T: Hash + Eq + PartialEq> {
//     conn_ident: T,
//     inner: Arc<APIConnectionManagerImpl<T>>
// }

// impl<T: Hash + Eq + PartialEq> Drop for ClientConnection<T> {
//     fn drop(&mut self) {
//         self.inner.connections.lock().remove(&self.conn_ident);
//     }
// }


#[derive(Clone)]
pub struct APIConnectionManager {
    inner: Arc<APIConnectionManagerImpl>,
}

impl APIConnectionManager
{
    pub fn new(ping_delay: Duration) -> Self {
        Self {
            inner: Arc::new(APIConnectionManagerImpl {
                // connections: Default::default(),
                ping_delay,
            }),
        }
    }

    fn init_connection<FirstConnectionState, SessionState, M>(
        &self,
        ws: WebSocketUpgrade,
        session_state: FirstConnectionState,
    ) -> Response
    where
        FirstConnectionState: Send + Sync + 'static,
        SessionState: Send + Sync + 'static,
        M: APIMessage<SessionState = SessionState, FirstConnectionState = FirstConnectionState>,
    {
        // let client_id = session_state.get_client_identifier();
        // {
        //     let mut lock = self.inner.connections.lock();
        //     if lock.contains(&client_id) {
        //         return (StatusCode::CONFLICT, "Already connected").into_response()
        //     }
        //     lock.insert(client_id.clone());
        // }

        let inner = self.inner.clone();

        ws.on_upgrade(|ws| async move {
            // let _client_conn = ClientConnection {
            //     conn_ident: client_id,
            //     inner: inner.clone(),
            // };
            let mut ws = ManagedWebSocket::new(ws, inner.ping_delay);
            macro_rules! unwrap_ws {
                ($ws: expr) => {
                    let Some(ws_tmp) = $ws else {
                                            return
                                        };
                    ws = ws_tmp;
                };
            }
            macro_rules! recv {
                () => {{
                    let Some((tmp_ws, msg)) = ws.recv().await else { break };
                    ws = tmp_ws;
                    msg
                }};
            }
            macro_rules! send {
                ($msg: expr) => {
                    unwrap_ws!(ws.send($msg))
                };
            }

            let Some((ws_tmp, mut session_state)) = M::on_connection(session_state, ws).await else {
                return
            };
            ws = ws_tmp;

            loop {
                let Message::Text(msg) = recv!() else { continue };
                let msg = match M::try_from(msg) {
                    Ok(msg) => msg,
                    Err(e) => {
                        send!(e);
                        continue;
                    }
                };

                unwrap_ws!(msg.route(&mut session_state, ws).await);
            }
        })
    }
}


// pub trait ClientIdentifier {
//     type ClientID: Hash + Eq + PartialEq + Clone + Send + Sync + 'static;

//     fn get_client_identifier(&self) -> Option<Self::ClientID>;
// }


async fn ws_api_route_internal<FirstConnectionState, SessionState, M, S, B>(
    // WSAPIRequest { upgrade, client_conn, session_state, conn_manager }: WSAPIRequest<FirstConnectionState>
    upgrade: WebSocketUpgrade,
    State(conn_manager): State<APIConnectionManager>,
    session_state: FirstConnectionState
) -> Response
where
    FirstConnectionState: FromRequest<S, B> + Send + Sync + 'static,
    SessionState: Send + Sync + 'static,
    M: APIMessage<SessionState = SessionState, FirstConnectionState = FirstConnectionState>,
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    APIConnectionManager: FromRef<S>,
{

    conn_manager.init_connection::<FirstConnectionState, SessionState, M>(
        upgrade,
        session_state,
    )
}

pub fn ws_api_route<FirstConnectionState, SessionState, M, S, B>() -> MethodRouter<S, B>
where
    FirstConnectionState: FromRequest<S, B> + Send + Sync + 'static,
    SessionState: Send + Sync + 'static,
    M: APIMessage<SessionState = SessionState, FirstConnectionState = FirstConnectionState>,
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    APIConnectionManager: FromRef<S>,
{
    axum::routing::get(ws_api_route_internal::<FirstConnectionState, SessionState, M, S, B>)
}
