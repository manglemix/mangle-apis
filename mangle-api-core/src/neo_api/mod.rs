use std::{sync::Arc, time::Duration};

use axum::{
    extract::{FromRef, State, WebSocketUpgrade},
    response::Response,
    routing::MethodRouter,
};
use messagist::{text::JsonMessageStream, MessageHandler};

use crate::ws::ManagedWebSocket;

struct NeoApiConfigImpl<H> {
    ping_delay: Duration,
    handler: H,
}

#[derive(Clone)]
pub struct NeoApiConfig<H: MessageHandler + Send + Sync + Clone> {
    inner: Arc<NeoApiConfigImpl<H>>,
}

impl<H: MessageHandler + Send + Sync + Clone> NeoApiConfig<H> {
    pub fn new(ping_delay: Duration, handler: H) -> Self {
        Self {
            inner: Arc::new(NeoApiConfigImpl {
                ping_delay,
                handler,
            }),
        }
    }
}

async fn ws_api_route_internal<S, H>(
    ws: WebSocketUpgrade,
    State(config): State<NeoApiConfig<H>>,
) -> Response
where
    S: Send + Sync + Clone + 'static,
    H: MessageHandler + Send + Sync + Clone + 'static,
    NeoApiConfig<H>: FromRef<S>,
{
    ws.on_upgrade(move |ws| async move {
        config
            .inner
            .handler
            .clone()
            .handle(JsonMessageStream::from(ManagedWebSocket::new(
                ws,
                config.inner.ping_delay,
            )))
            .await;
    })
}

pub fn ws_api_route<S, B, H>() -> MethodRouter<S, B>
where
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    H: MessageHandler + Send + Sync + Clone + 'static,
    NeoApiConfig<H>: FromRef<S>,
{
    axum::routing::get(ws_api_route_internal::<S, H>)
}
