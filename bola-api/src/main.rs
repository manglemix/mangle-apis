use mangle_api_core::{
    make_app,
    start_api,
    anyhow,
    tokio,
    auth::oauth2::{
        OAuthState,
        google::{GoogleOAuth, new_google_oauth_from_file, GAuthContainer},
        github::{GithubOAuth, new_github_oauth_from_file, GithubOAuthContainer},
        initiate_oauth,
        GOOGLE_PROFILE_SCOPES,
        OAuthStateContainer,
        oauth_redirect
    },
    pre_matches,
    get_pipe_name,
    axum::{
        self,
        extract::{State, WebSocketUpgrade},
        routing::get,
        response::Response
    }
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

    let state = GlobalState {
        oauth_state: Default::default(),
        gclient: new_google_oauth_from_file(&config.google_client_secret_path)?,
        github_client: new_github_oauth_from_file(&config.github_client_secret_path)?
    };

    async fn test_oauth(
        ws: WebSocketUpgrade,
        State(oauth_state): State<OAuthState>,
        State(client): State<GoogleOAuth>
    ) -> Response {
        initiate_oauth(ws, oauth_state, client, GOOGLE_PROFILE_SCOPES)
    }

    start_api(
        state,
        app,
        pipe_name,
        config,
        [
            "^/oauth",
            "^/gauth$"
        ],
        [
            ("/oauth/redirect", oauth_redirect!()),
            ("/gauth", get(test_oauth))
        ]
    ).await
}
