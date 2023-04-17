#![feature(trivial_bounds)]
#![feature(string_leak)]
#![feature(map_try_insert)]
#![feature(vec_push_within_capacity)]
#![feature(never_type)]

use std::{fs::read_to_string, sync::Arc, time::Duration};

use anyhow::{self, Context};
use axum::{
    async_trait,
    extract::FromRef,
    http::{HeaderValue, StatusCode},
    response::Response,
};
use control::new_control_handler;
use db::DB;
use leaderboard::Leaderboard;
use log::info;
use mangle_api_core::{
    auth::{
        auth_pages::{AuthPages, AuthPagesSrc},
        openid::{
            google::{new_google_oidc_from_file, GoogleOIDC},
            openid_redirect, OIDCState,
        },
        token::{HeaderTokenGranterConfig, TokenGranter, TokenGranterConfig},
    },
    distributed::Node,
    get_https_credentials,
    get_pipe_name,
    make_app,
    new_api,
    // neo_api::{ws_api_route},
    pre_matches,
    setup_logger,
    CommandMatchResult,
};
use messagist::{pipes::start_connection, MessageStream};
use multiplayer::Multiplayer;
use tokio::{self};

mod config;
mod control;
mod db;
mod leaderboard;
mod multiplayer;
mod network;
mod tournament;
mod ws_api;

use config::Config;
use tournament::Tournament;

use crate::control::ControlClientMessage;
// use ws_api::{FirstConnectionState, SessionState, WSAPIMessage};

#[derive(Clone, PartialEq, Eq, Hash)]
struct LoginTokenData {
    username: String,
    email: String,
}

const WS_PING_DELAY: Duration = Duration::from_secs(45);

enum LoginTokenConfig {}

impl TokenGranterConfig for LoginTokenConfig {
    type TokenDistinguisher = LoginTokenData;
    const TOKEN_LENGTH: usize = 32;
}

impl HeaderTokenGranterConfig for LoginTokenConfig {
    const HEADER_NAME: &'static str = "Login-Token";
}

type LoginTokenGranter = TokenGranter<LoginTokenConfig>;

#[derive(Clone, FromRef)]
struct GlobalState {
    db: DB,
    oidc_state: OIDCState,
    goidc: GoogleOIDC,
    auth_pages: AuthPages,
    login_tokens: LoginTokenGranter,
    leaderboard: Leaderboard,
    // api_conn_manager: APIConnectionManager<Arc<String>>,
    tournament: Tournament,
    multiplayer: Multiplayer,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = make_app("BolaAPI", env!("CARGO_PKG_VERSION"), "The API for Bola", []);
    let matches = app.get_matches();

    let pipe_name = get_pipe_name("BOLA_SOCKET_NAME", "/dev/bola_server.sock");

    let config = match pre_matches::<Config>(&matches, pipe_name.as_os_str(), None).await? {
        CommandMatchResult::StartProgram(x) => x,
        CommandMatchResult::Unmatched(x) => match x {
            ("stop", _) => {
                let mut conn = start_connection(pipe_name)
                    .await
                    .context("Connecting to server")?;
                conn.send_message(&ControlClientMessage::Stop)
                    .await
                    .context("Sending Stop to server")?;
                println!("Stop command issued...");
                conn.wait_for_error().await;
                println!("Server stopped succesfully");
                return Ok(());
            }
            _ => unreachable!(),
        },
    };

    let builder = aws_config::from_env();
    #[cfg(debug_assertions)]
    let builder = builder.region(aws_types::region::Region::from_static("us-east-2"));
    let aws_config = builder.load().await;

    setup_logger(
        &config.stderr_log,
        &config.routing_log,
        &config.security_log,
    )?
    .apply()
    .context("Setting up logger")?;

    let https_der = if config.https {
        if config.https_domain.is_empty() {
            return Err(anyhow::Error::msg(
                "https is true, but https_domain is empty",
            ));
        }
        let tmp = Some(
            get_https_credentials(
                config.bind_address.clone(),
                &config.certs_path,
                &config.key_path,
                "shabouza030@gmail.com".into(),
                config.https_domain,
            )
            .await?,
        );
        info!("HTTPS certificates loaded successfully");
        tmp
    } else {
        None
    };

    let css = read_to_string(&config.stylesheet_path)
        .context(format!("Reading {}", config.stylesheet_path))?;
    let internal_error_page = read_to_string(&config.internal_error_path)
        .context(format!("Reading {}", config.internal_error_path))?;
    let invalid_page =
        read_to_string(&config.invalid_path).context(format!("Reading {}", config.invalid_path))?;
    let success_page =
        read_to_string(&config.success_path).context(format!("Reading {}", config.success_path))?;
    let late_page =
        read_to_string(&config.late_path).context(format!("Reading {}", config.late_path))?;

    let oidc_state = OIDCState::default();

    let node = Node::new(
        config.sibling_domains,
        config.network_port,
        https_der.clone(),
    )
    .await?;
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
            internal_error: internal_error_page,
            late: late_page,
            invalid: invalid_page,
            success: success_page,
        }),
        oidc_state,
        login_tokens: LoginTokenGranter::new(config.token_duration),
        leaderboard: Leaderboard::new(db.clone(), node, 5).await?,
        db,
        // api_conn_manager: APIConnectionManager::new(WS_PING_DELAY),
        tournament: Tournament::new(config.start_week_time),
        multiplayer: Multiplayer::default(),
    };

    let (control_handler, control_handler_recv) = new_control_handler();

    let api = new_api()
        .set_state(state)
        .set_pipe_name(pipe_name)
        .set_api_token(HeaderValue::from_str(&config.api_token).context("parsing api_token")?)
        .set_bind_address(config.bind_address)
        .set_cors_allowed_methods({
            let mut out = Vec::new();

            config
                .cors_allowed_methods
                .into_iter()
                .try_for_each(|x| x.parse().map(|x| out.push(x)))?;

            out
        })
        .set_cors_allowed_origins({
            let mut out = Vec::new();

            config
                .cors_allowed_origins
                .into_iter()
                .try_for_each(|x| x.parse().map(|x| out.push(x)))?;

            out
        })
        .set_public_paths(["^/oidc/", "^/manglemix.css$", "^/$"])
        .set_routes([
            ("/oidc/redirect", openid_redirect()),
            (
                "/manglemix.css",
                axum::routing::get(|| async move {
                    Response::builder()
                        .header("Content-Type", "text/css")
                        .body(css.clone())
                        .unwrap()
                }),
            ),
            // (
            //     "/ws_api",
            //     ws_api_route::<FirstConnectionState, SessionState, WSAPIMessage, _, _, _>(),
            // ),
            (
                "/",
                axum::routing::get(|| async move {
                    Response::builder()
                        .header("Location", "https://bola.manglemix.com")
                        .status(StatusCode::TEMPORARY_REDIRECT)
                        .body(String::new())
                        .unwrap()
                }),
            ),
        ])
        .set_control_handler(control_handler)
        .set_concurrent_future(control_handler_recv);

    if let Some(https_der) = https_der {
        api.set_https_identity(https_der).run().await
    } else {
        api.run().await
    }
}
