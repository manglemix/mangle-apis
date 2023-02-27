use std::{borrow::Borrow, collections::HashSet, hash::Hash, sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{ws::Message, FromRef, FromRequest, State, WebSocketUpgrade},
    response::Response,
    routing::MethodRouter,
};
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

#[derive(Clone)]
pub struct ConnectionLock<T: Hash + Eq> {
    id: T,
    inner: Arc<APIConnectionManagerImpl<T>>,
}

impl<T: Hash + Eq> Drop for ConnectionLock<T> {
    fn drop(&mut self) {
        self.inner.connections.lock().remove(&self.id);
    }
}

struct APIConnectionManagerImpl<T> {
    connections: Mutex<HashSet<T>>,
    ping_delay: Duration,
}

#[derive(Clone)]
pub struct APIConnectionManager<T: Hash + Eq + Clone + Send + Sync + 'static = ()> {
    inner: Arc<APIConnectionManagerImpl<T>>,
}

impl<T: Hash + Eq + Clone + Send + Sync + 'static> APIConnectionManager<T> {
    pub fn new(ping_delay: Duration) -> Self {
        Self {
            inner: Arc::new(APIConnectionManagerImpl {
                connections: Default::default(),
                ping_delay,
            }),
        }
    }

    pub fn is_connection_locked(&self, id: impl Borrow<T>) -> bool {
        self.inner.connections.lock().contains(id.borrow())
    }

    pub fn get_connection_lock(&self, id: T) -> Result<ConnectionLock<T>, T> {
        let mut lock = self.inner.connections.lock();
        if lock.contains(&id) {
            Err(id)
        } else {
            lock.insert(id.clone());
            Ok(ConnectionLock {
                id,
                inner: self.inner.clone(),
            })
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
        let inner = self.inner.clone();

        ws.on_upgrade(|ws| async move {
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

async fn ws_api_route_internal<FirstConnectionState, SessionState, M, S, B, ID>(
    // WSAPIRequest { upgrade, client_conn, session_state, conn_manager }: WSAPIRequest<FirstConnectionState>
    upgrade: WebSocketUpgrade,
    State(conn_manager): State<APIConnectionManager<ID>>,
    session_state: FirstConnectionState,
) -> Response
where
    FirstConnectionState: FromRequest<S, B> + Send + Sync + 'static,
    SessionState: Send + Sync + 'static,
    M: APIMessage<SessionState = SessionState, FirstConnectionState = FirstConnectionState>,
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    APIConnectionManager<ID>: FromRef<S>,
    ID: Hash + Eq + Clone + Send + Sync + 'static,
{
    conn_manager.init_connection::<FirstConnectionState, SessionState, M>(upgrade, session_state)
}

pub fn ws_api_route<FirstConnectionState, SessionState, M, S, B, ID>() -> MethodRouter<S, B>
where
    FirstConnectionState: FromRequest<S, B> + Send + Sync + 'static,
    SessionState: Send + Sync + 'static,
    M: APIMessage<SessionState = SessionState, FirstConnectionState = FirstConnectionState>,
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    APIConnectionManager<ID>: FromRef<S>,
    ID: Hash + Eq + Clone + Send + Sync + 'static,
{
    axum::routing::get(ws_api_route_internal::<FirstConnectionState, SessionState, M, S, B, ID>)
}
