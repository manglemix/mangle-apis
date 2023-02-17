// use aws_config::{SdkConfig};
use db::{DB, GetUserProfileError, UserProfile};
use mangle_api_core::{
    anyhow::{self, Context},
    auth::{
        auth_pages::{AuthPages, AuthPagesContainer, AuthPagesSrc},
        openid::{
            google::{new_google_oidc_from_file, GOIDCContainer, GoogleOIDC},
            OIDCState, OIDCStateContainer,
        },
    },
    axum::{
        extract::{ws::Message, FromRef, State, WebSocketUpgrade},
        http::HeaderValue,
        response::Response,
        routing::get,
    },
    get_pipe_name, make_app, openid_redirect, pre_matches, start_api,
    tokio::{self, select},
    ws::{PolledWebSocket, WsExt},
    BaseConfig, BindAddress,
};

mod config;
mod db;

use config::Config;
use rustrict::CensorStr;

use crate::db::GetItemError;

#[derive(Clone)]
struct GlobalState {
    db: DB,
    oidc_state: OIDCState,
    goidc: GoogleOIDC,
    auth_pages: AuthPages,
}

impl FromRef<GlobalState> for DB {
    fn from_ref(input: &GlobalState) -> Self {
        input.db.clone()
    }
}

impl GOIDCContainer for GlobalState {
    fn get_goidc(&self) -> &GoogleOIDC {
        &self.goidc
    }
}

impl AuthPagesContainer for GlobalState {
    fn get_auth_pages(&self) -> &AuthPages {
        &self.auth_pages
    }
}

impl OIDCStateContainer for GlobalState {
    fn get_oidc_state(&self) -> &OIDCState {
        &self.oidc_state
    }
}

async fn login(State(GoogleOIDC(oidc)): State<GoogleOIDC>, State(db): State<DB>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|ws| async move {
        let (auth_url, fut) = oidc.initiate_auth(["openid", "email"]);
        let Some(ws) = ws.easy_send(Message::Text(auth_url.to_string())).await else { return };

        let mut ws = PolledWebSocket::new(ws);
        let opt = select! {
            opt = fut => { opt }
            () = ws.wait_for_failure() => { return }
        };
        let Some(mut ws) = ws.into_inner().await else { return };

        let Some(data) = opt else {
            ws.final_send_close_frame(1000, "Auth Failed").await;
            return
        };
        let Some(email) = data.email else {
            ws.close_bad_payload().await;
            return
        };

        match db.get_user_profile(&email).await {
            Ok(x) => {
                let Some(ws_tmp) = ws.easy_send(Message::Text(x.username)).await else { return };
                ws = ws_tmp;
            }
            Err(GetUserProfileError::GetItemError(GetItemError::ItemNotFound)) => {
                let Some(ws_tmp) = ws.easy_send(Message::Text("Sign Up".into())).await else { return };
                let Some((ws_tmp, msg)) = ws_tmp.easy_recv().await else { return };
                ws = ws_tmp;

                let Message::Text(username) = msg else {
                    ws.final_send_close_frame(1007, "Expected username").await;
                    return
                };

                if username.is_inappropriate() {
                    ws.final_send_close_frame(1007, "Inappropriate username").await;
                    return
                }

                let profile = UserProfile {
                    username,
                    ..Default::default()
                };

                if let Err(_) = db.create_user_profile(email, &profile).await {
                    ws.close_internal_error().await;
                    return
                }
            }
            Err(e) => {
                eprintln!("{e:?}");
                ws.close_internal_error().await;
                return
            }
        };

        ws.final_send_close_frame(1000, "Auth Succeeded!").await;
    })
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = make_app("BolaAPI", env!("CARGO_PKG_VERSION"), "The API for Bola", []);

    let pipe_name = get_pipe_name("BOLA_PIPE_NAME", "bola_pipe");

    let Some(config) = pre_matches::<Config>(app.clone(), &pipe_name).await? else {
        return Ok(())
    };

    #[cfg(debug_assertions)]
    let aws_config = aws_types::sdk_config::Builder::default()
        .region(aws_sdk_dynamodb::Region::new("us-west-1"))
        // .credentials_cache(CredentialsCache::lazy())
        .build();

    #[cfg(not(debug_assertions))]
    let aws_config = aws_config::load_from_env().await;

    let oidc_state = OIDCState::default();

    let state = GlobalState {
        db: DB::new(&aws_config, config.bola_profiles_table),
        goidc: new_google_oidc_from_file(
            config.google_client_secret_path,
            oidc_state.clone(),
            &config.oidc_redirect,
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
            // ("/sign_up", post(sign_up)),
            ("/login", get(login)),
        ],
    )
    .await
}
