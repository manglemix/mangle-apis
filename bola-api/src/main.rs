use mangle_api_core::{
    make_app,
    start_api,
    anyhow,
    tokio,
    auth::oauth2::{
        OAuthState,
        google::{GoogleOAuth, new_google_oauth_from_file, GAuthContainer},
        github::{GithubOAuth, new_github_oauth_from_file, GithubOAuthContainer},
        GOOGLE_PROFILE_SCOPES,
        OAuthStateContainer,
        oauth_redirect
    },
    pre_matches,
    get_pipe_name,
    axum::{
        extract::{State, WebSocketUpgrade, ws::Message},
        routing::get,
        response::Response
    }, ws::PolledWebSocket
};

mod config;

use config::Config;


#[derive(Clone)]
struct GlobalState {
    oauth_state: OAuthState,
    gclient: GoogleOAuth,
    github_client: GithubOAuth
}


impl OAuthStateContainer for GlobalState {
    fn get_oauth_state(&self) -> &OAuthState {
        &self.oauth_state
    }
}


impl GAuthContainer for GlobalState {
    fn get_gauth_state(&self) -> &GoogleOAuth {
        &self.gclient
    }
}


impl GithubOAuthContainer for GlobalState {
    fn get_github_auth_state(&self) -> &GithubOAuth {
        &self.github_client
    }
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = make_app(
        "BolaAPI",
        env!("CARGO_PKG_VERSION"),
        "The API for Bola",
        []
    );

    let pipe_name = get_pipe_name("BOLA_PIPE_NAME", "bola_pipe");

    let Some(config) = pre_matches::<Config>(app.clone(), &pipe_name).await? else {
        return Ok(())
    };

    let oauth_state: OAuthState = Default::default();
    let state = GlobalState {
        gclient: new_google_oauth_from_file(&config.google_client_secret_path, oauth_state.clone())?,
        github_client: new_github_oauth_from_file(&config.github_client_secret_path, oauth_state.clone())?,
        oauth_state
    };

    async fn test_oauth(
        ws: WebSocketUpgrade,
        State(client): State<GoogleOAuth>
    ) -> Response {
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

            let _ = if let Some(_token) = opt {
                ws.send(Message::Text("authed".into()))
            } else {
                ws.send(Message::Text("auth failed".into()))
            }.await;
        })
    }

    start_api(
        state,
        app,
        pipe_name,
        config,
        [
            "^/oauth/redirect$",
            "^/gauth$"
        ],
        [
            ("/oauth/redirect", oauth_redirect!()),
            ("/gauth", get(test_oauth))
        ]
    ).await
}
