use std::{time::Duration};

use aws_config::load_from_env;
use aws_sdk_dynamodb::{Client, model::AttributeValue};
use mangle_api_core::{
    make_app,
    start_api,
    anyhow::{self},
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
    },
};

mod config;
mod users;

use config::Config;
use users::{login, UserProfileTokens};


#[derive(Clone)]
struct GlobalState {
    oauth_state: OAuthState,
    gclient: GoogleOAuth,
    github_client: GithubOAuth,
    user_tokens: UserProfileTokens,
    // db_client: RedisClient
}


impl FromRef<GlobalState> for UserProfileTokens {
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
    let client = Client::new(&load_from_env().await);
    println!("{:?}", client.get_item().set_table_name(Some("bola_profiles".into())).key("email", AttributeValue::S("test3".into())).send().await?);
    return Ok(());

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
        oauth_state,
        user_tokens: UserProfileTokens::new(Duration::from_secs(config.token_duration as u64)),
        // db_client
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
