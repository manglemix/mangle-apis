use mangle_api_core::{make_app, start_api, anyhow, tokio, auth::oauth2::{OAuthState, google::{GoogleOAuth, new_google_oauth_from_file}, github::{GithubOAuth, new_github_oauth_from_file}}, pre_matches, get_pipe_name};

mod config;

use config::Config;


#[derive(Clone)]
struct GlobalState {
    oauth_state: OAuthState,
    gclient: GoogleOAuth,
    github_client: GithubOAuth
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

    start_api(
        state,
        app,
        pipe_name,
        config,
        [],
        []
    ).await
}
