use aws_config::load_from_env;
use db::DB;
use mangle_api_core::{
    make_app,
    start_api,
    anyhow::{self, Context},
    tokio,
    pre_matches,
    get_pipe_name,
    axum::{extract::FromRef, http::HeaderValue, routing::get}, BaseConfig, BindAddress,
};

mod config;
mod db;
mod user_auth;

use config::Config;
use user_auth::{UserAuth, sign_up};


#[derive(Clone)]
struct GlobalState {
    db: DB,
    user_auth: UserAuth
}


impl FromRef<GlobalState> for DB {
    fn from_ref(input: &GlobalState) -> Self {
        input.db.clone()
    }
}


impl FromRef<GlobalState> for UserAuth {
    fn from_ref(input: &GlobalState) -> Self {
        input.user_auth.clone()
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

    let aws_config = load_from_env().await;

    let state = GlobalState {
        db: DB::new(&aws_config),
        user_auth: UserAuth::new(&aws_config, config.cognito_client_id)
    };

    let config = BaseConfig {
        api_token: HeaderValue::from_str(&config.api_token).context("parsing api_token")?,
        bind_address: format!("{}:{}", config.server_address, config.server_port)
                        .parse()
                        .map(BindAddress::Network)
                        .context("parsing server_address and server_port")?,
        stderr_log_path: config.stderr_log,
        routing_log_path: config.routing_log,
        suspicious_security_log_path: config.suspicious_security_log,
        cors_allowed_methods: {
            let mut out = Vec::new();

            config.cors_allowed_methods
                .into_iter()
                .try_for_each(|x| {
                    x.parse().map(|x| out.push(x))
                })?;

            out
        },
        cors_allowed_origins: {
            let mut out = Vec::new();

            config.cors_allowed_origins
                .into_iter()
                .try_for_each(|x| {
                    x.parse().map(|x| out.push(x))
                })?;

            out
        }
    };

    start_api(
        state,
        app,
        pipe_name,
        config,
        [

        ],
        [
            ("/sign_up", get(sign_up))
        ]
    ).await
}
