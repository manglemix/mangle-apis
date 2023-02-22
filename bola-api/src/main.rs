#![feature(trivial_bounds)]

use std::{net::ToSocketAddrs, time::Duration};

// use aws_config::{SdkConfig};
use anyhow::{self, Context};
use axum::{extract::FromRef, http::HeaderValue, routing::get};
use db::DB;
use leaderboard::{
    get_easy_leaderboard, get_expert_leaderboard, get_normal_leaderboard, Leaderboard,
};
use mangle_api_core::{
    auth::{
        auth_pages::{AuthPages, AuthPagesSrc},
        openid::{
            google::{new_google_oidc_from_file, GoogleOIDC},
            OIDCState,
        },
    },
    create_header_token_granter,
    distributed::Node,
    get_pipe_name, make_app, openid_redirect, pre_matches, start_api, BaseConfig, BindAddress,
};
use tokio::{self};

mod config;
mod db;
mod leaderboard;
mod network;
mod user_auth;

const WS_PING_DELAY: Duration = Duration::from_secs(45);

use config::Config;

#[derive(Clone, PartialEq, Eq, Hash)]
struct LoginTokenData {
    username: String,
    email: String,
}

create_header_token_granter!( LoginTokenGranter "Login-Token" 32 LoginTokenData);

#[derive(Clone, FromRef)]
struct GlobalState {
    db: DB,
    oidc_state: OIDCState,
    goidc: GoogleOIDC,
    auth_pages: AuthPages,
    login_tokens: LoginTokenGranter,
    // node: Node<NetworkMessage>,
    leaderboard: Leaderboard,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = make_app("BolaAPI", env!("CARGO_PKG_VERSION"), "The API for Bola", []);

    let pipe_name = get_pipe_name("BOLA_PIPE_NAME", "bola_pipe");

    let Some(config) = pre_matches::<Config>(app.clone(), &pipe_name).await? else {
        return Ok(())
    };

    let builder = aws_config::from_env();
    #[cfg(debug_assertions)]
    let builder = builder.region(aws_types::region::Region::from_static("us-east-2"));
    let aws_config = builder.load().await;

    let oidc_state = OIDCState::default();

    let node = Node::new(config.sibling_domains, config.network_port).await?;
    let db = DB::new(&aws_config, config.bola_profiles_table);

    let state = GlobalState {
        goidc: new_google_oidc_from_file(
            config.google_client_secret_path,
            oidc_state.clone(),
            &(config.oidc_redirect_base + "/oidc/redirect"),
        )
        .await
        .context("parsing google oauth")?,
        auth_pages: AuthPages::new(AuthPagesSrc {
            internal_error: "Internal Error".into(),
            late: "Late".into(),
            invalid: "Invalid".into(),
            success: "Success".into(),
        }),
        oidc_state,
        login_tokens: LoginTokenGranter::new(Duration::from_secs(config.token_duration)),
        leaderboard: Leaderboard::new(db.clone(), node, 5).await?,
        // node,
        db,
    };

    let config = BaseConfig {
        api_token: HeaderValue::from_str(&config.api_token).context("parsing api_token")?,
        bind_address: (config.server_address, config.server_port)
            .to_socket_addrs()
            .map(|mut x| BindAddress::Network(x.next().expect("At least 1 socket addr")))
            .context("parsing server_address and server_port")?,
        stderr_log_path: config.stderr_log,
        routing_log_path: config.routing_log,
        suspicious_security_log_path: config.suspicious_security_log,
        cors_allowed_methods: {
            let mut out = Vec::new();

            config
                .cors_allowed_methods
                .into_iter()
                .try_for_each(|x| x.parse().map(|x| out.push(x)))?;

            out
        },
        cors_allowed_origins: {
            let mut out = Vec::new();

            config
                .cors_allowed_origins
                .into_iter()
                .try_for_each(|x| x.parse().map(|x| out.push(x)))?;

            out
        },
    };

    start_api(
        state,
        app,
        pipe_name,
        config,
        ["^/oidc/"],
        [
            ("/oidc/redirect", openid_redirect!()),
            ("/login", get(user_auth::login)),
            ("/quick_login", get(user_auth::quick_login)),
            ("/leaderboard/easy", get(get_easy_leaderboard)),
            ("/leaderboard/normal", get(get_normal_leaderboard)),
            ("/leaderboard/expert", get(get_expert_leaderboard)),
        ],
    )
    .await
}
