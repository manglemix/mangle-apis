use std::{sync::Arc, time::Duration};

use axum::{
    extract::{FromRef, FromRequest, State, WebSocketUpgrade},
    response::Response,
    routing::MethodRouter,
};
use messagist::{text::JsonMessageStream, ExclusiveMessageHandler};

use crate::ws::ManagedWebSocket;

struct NeoApiConfigImpl<H> {
    ping_delay: Duration,
    handler: H,
}

#[derive(Clone)]
pub struct NeoApiConfig<H: ExclusiveMessageHandler + Send + Sync + Clone> {
    inner: Arc<NeoApiConfigImpl<H>>,
}

impl<H: ExclusiveMessageHandler + Send + Sync + Clone> NeoApiConfig<H> {
    pub fn new(ping_delay: Duration, handler: H) -> Self {
        Self {
            inner: Arc::new(NeoApiConfigImpl {
                ping_delay,
                handler,
            }),
        }
    }
    pub fn get_handler(&self) -> &H {
        &self.inner.handler
    }
}

async fn ws_api_route_internal<S, B, H, R>(
    ws: WebSocketUpgrade,
    State(config): State<NeoApiConfig<H>>,
    request: R,
) -> Response
where
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    H: ExclusiveMessageHandler<SessionState = R> + Send + Sync + Clone + 'static,
    NeoApiConfig<H>: FromRef<S>,
    R: FromRequest<S, B> + Send + Sync + 'static,
{
    ws.on_upgrade(move |ws| async move {
        config
            .inner
            .handler
            .clone()
            .handle(
                JsonMessageStream::from(ManagedWebSocket::new(ws, config.inner.ping_delay)),
                request,
            )
            .await;
    })
}

pub fn ws_api_route<S, B, H, R>() -> MethodRouter<S, B>
where
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    H: ExclusiveMessageHandler<SessionState = R> + Send + Sync + Clone + 'static,
    NeoApiConfig<H>: FromRef<S>,
    R: FromRequest<S, B> + Send + Sync + 'static,
{
    axum::routing::get(ws_api_route_internal::<S, B, H, R>)
}
