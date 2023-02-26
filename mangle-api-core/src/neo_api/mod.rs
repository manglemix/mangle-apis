use std::{collections::HashSet, net::{SocketAddr, IpAddr}, sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{ws::Message, ConnectInfo, FromRef, FromRequest, WebSocketUpgrade, FromRequestParts},
    response::{Response, IntoResponse},
    routing::MethodRouter,
};
use hyper::{StatusCode, Request};
use parking_lot::Mutex;

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
    connections: Mutex<HashSet<IpAddr>>,
    ping_delay: Duration,
}


struct ClientConnection {
    client_addr: IpAddr,
    inner: Arc<APIConnectionManagerImpl>
}

impl Drop for ClientConnection {
    fn drop(&mut self) {
        self.inner.connections.lock().remove(&self.client_addr);
    }
}


#[derive(Clone)]
pub struct APIConnectionManager {
    inner: Arc<APIConnectionManagerImpl>,
}

impl APIConnectionManager {
    pub fn new(ping_delay: Duration) -> Self {
        Self {
            inner: Arc::new(APIConnectionManagerImpl {
                connections: Default::default(),
                ping_delay,
            }),
        }
    }

    fn init_connection<FirstConnectionState, SessionState, M>(
        &self,
        ws: WebSocketUpgrade,
        client_conn: ClientConnection,
        session_state: FirstConnectionState,
    ) -> Response
    where
        FirstConnectionState: Send + Sync + 'static,
        SessionState: Send + Sync + 'static,
        M: APIMessage<SessionState = SessionState, FirstConnectionState = FirstConnectionState>,
    {
        let inner = self.inner.clone();

        ws.on_upgrade(|ws| async move {
            let _client_conn = client_conn;
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


struct WSAPIRequest<S> {
    upgrade: WebSocketUpgrade,
    conn_manager: APIConnectionManager,
    client_conn: ClientConnection,
    session_state: S
}


#[async_trait]
impl<S, State, B> FromRequest<State, B> for WSAPIRequest<S>
where
    APIConnectionManager: FromRef<State>,
    S: FromRequest<State, B>,
    State: Send + Sync + 'static,
    B: Send + Sync + 'static
{
    type Rejection = Response;

    async fn from_request(req: Request<B>, state: &State) -> Result<Self, Self::Rejection> {
        let (mut parts, body) = req.into_parts();
        let connect_info: ConnectInfo<SocketAddr> = ConnectInfo::from_request_parts(&mut parts, state)
            .await.
            map_err(IntoResponse::into_response)?;
        
        let client_addr = connect_info.0.ip();
        let conn_manager = APIConnectionManager::from_ref(state);
        {
            let mut lock = conn_manager.inner.connections.lock();
            if lock.contains(&client_addr) {
                return Err((StatusCode::CONFLICT, "Already connected").into_response());
            }
            lock.insert(client_addr);
        }
        Ok(Self {
            upgrade: WebSocketUpgrade::from_request_parts(&mut parts, state)
                .await
                .map_err(IntoResponse::into_response)?,

            session_state: S::from_request(Request::from_parts(parts, body), state)
                .await
                .map_err(IntoResponse::into_response)?,

            client_conn: ClientConnection {
                client_addr,
                inner: conn_manager.inner.clone()
            },
            conn_manager
        })
    }
}


async fn ws_api_route_internal<FirstConnectionState, SessionState, M, S, B>(
    WSAPIRequest { upgrade, client_conn, session_state, conn_manager }: WSAPIRequest<FirstConnectionState>
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
        client_conn,
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
