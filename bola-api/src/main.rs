use std::{time::Duration, mem::take};

use db::DBClient;
use mangle_api_core::{
    make_app,
    start_api,
    anyhow::{self, Error, Context},
    tokio,
    auth::{oauth2::{
        OAuthState,
        google::{GoogleOAuth, new_google_oauth_from_file, GAuthContainer},
        github::{GithubOAuth, new_github_oauth_from_file, GithubOAuthContainer},
        OAuthStateContainer,
        oauth_redirect
    }},
    pre_matches,
    get_pipe_name,
    axum::{
        extract::{FromRef},
        routing::get,
    }
};

mod config;
mod db;
mod users;

use config::Config;
use users::{login, UserTokens};


#[derive(Clone)]
struct GlobalState {
    oauth_state: OAuthState,
    gclient: GoogleOAuth,
    github_client: GithubOAuth,
    user_tokens: UserTokens,
    db_client: DBClient
}


impl FromRef<GlobalState> for DBClient {
    fn from_ref(input: &GlobalState) -> Self {
        input.db_client.clone()
    }
}


impl FromRef<GlobalState> for UserTokens {
    fn from_ref(input: &GlobalState) -> Self {
        input.user_tokens.clone()
    }
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

    let Some(mut config) = pre_matches::<Config>(app.clone(), &pipe_name).await? else {
        return Ok(())
    };

    let db_client = DBClient::new(
        take(&mut config.redis_cluster_addrs),
        take(&mut config.redis_username),
        take(&mut config.redis_password)
    )
        .map_err(Into::<Error>::into)
        .context("Parsing redis_cluster_addrs")?;

    let oauth_state: OAuthState = Default::default();
    let state = GlobalState {
        gclient: new_google_oauth_from_file(&config.google_client_secret_path, oauth_state.clone())?,
        github_client: new_github_oauth_from_file(&config.github_client_secret_path, oauth_state.clone())?,
        oauth_state,
        user_tokens: UserTokens::new(Duration::from_secs(config.token_duration as u64)),
        db_client
    };

    start_api(
        state,
        app,
        pipe_name,
        config,
        [
            "^/oauth/redirect$"
        ],
        [
            ("/oauth/redirect", oauth_redirect!()),
            ("/login", get(login)),
        ]
    ).await
}
