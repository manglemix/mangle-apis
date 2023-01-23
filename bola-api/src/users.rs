use mangle_api_core::{create_header_token_granter, auth::{token::VerifiedToken, oauth2::{GOOGLE_PROFILE_SCOPES, google::GoogleOAuth}}, axum::{extract::{WebSocketUpgrade, State, ws::Message}, response::Response}, ws::PolledWebSocket};


create_header_token_granter!(pub UserTokens "UserToken" 32);
pub type UserToken = VerifiedToken<32, UserTokens>;


pub async fn login(ws: WebSocketUpgrade, State(client): State<GoogleOAuth>, State(token_granter): State<UserTokens>) -> Response {
    ws.on_upgrade(|mut ws| async move {
        let (url, fut) = client.initiate_auth(GOOGLE_PROFILE_SCOPES);
        
        if ws.send(Message::Text(url.to_string())).await.is_err() {
            return
        }

        let polled = PolledWebSocket::new(ws);
        let opt = fut.await;
        let mut ws = match polled.lock().await {
            Some(ws) => ws,
            None => return
        };

        if let Some(_token) = opt {
            let _ = ws.send(Message::Text(token_granter.create_token().into())).await;
        }
    })
}