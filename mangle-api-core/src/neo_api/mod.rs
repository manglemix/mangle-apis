use std::{time::Duration};

use axum::{
    extract::{FromRequest, State, WebSocketUpgrade},
    response::Response,
    routing::MethodRouter,
};
use messagist::{text::JsonMessageStream, AliasableMessageHandler};

use crate::ws::ManagedWebSocket;

pub struct NeoApiConfig<H: AliasableMessageHandler + Send + Sync> {
    ping_delay: Duration,
    handler: H,
}

impl<H: AliasableMessageHandler + Send + Sync> NeoApiConfig<H> {
    pub fn new(ping_delay: Duration, handler: H) -> Self {
        Self {
            ping_delay,
            handler,
        }
    }
    pub fn get_handler(&self) -> &H {
        &self.handler
    }
}

async fn ws_api_route_internal<S, B, H, R>(
    ws: WebSocketUpgrade,
    State(state): State<S>,
    request: R,
) -> Response
where
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    H: AliasableMessageHandler<SessionState = R> + Send + Sync + 'static,
    S: AsRef<NeoApiConfig<H>>,
    R: FromRequest<S, B> + Send + Sync + 'static,
{
    ws.on_upgrade(move |ws| async move {
        let config = state.as_ref();
        config
            .handler
            .handle(
                JsonMessageStream::from(ManagedWebSocket::new(ws, config.ping_delay)),
                request,
            )
            .await;
    })
}

pub fn ws_api_route<S, B, H, R>() -> MethodRouter<S, B>
where
    S: Send + Sync + Clone + 'static,
    B: Send + Sync + axum::body::HttpBody + 'static,
    H: AliasableMessageHandler<SessionState = R> + Send + Sync + 'static,
    S: AsRef<NeoApiConfig<H>>,
    R: FromRequest<S, B> + Send + Sync + 'static,
{
    axum::routing::get(ws_api_route_internal::<S, B, H, R>)
}
