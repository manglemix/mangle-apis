// use aws_config::{SdkConfig};
use aws_sdk_cognitoidentityprovider::Region;
use db::DB;
use mangle_api_core::{
    anyhow::{self, Context},
    auth::oauth2::{
        google::{new_google_oauth_from_file, GAuthContainer, GoogleOAuth},
        OAuthPages, OAuthPagesContainer, OAuthPagesSrc, OAuthState, OAuthStateContainer,
    },
    axum::{
        extract::{ws::Message, FromRef, State, WebSocketUpgrade},
        http::HeaderValue,
        response::Response,
        routing::get,
    },
    get_pipe_name, make_app, oauth_redirect, pre_matches, start_api,
    tokio::{self, select},
    ws::{PolledWebSocket, WsExt},
    BaseConfig, BindAddress,
};

mod config;
mod db;
mod user_auth;

use config::Config;
use user_auth::UserAuth;

#[derive(Clone)]
struct GlobalState {
    db: DB,
    user_auth: UserAuth,
    oauth_state: OAuthState,
    gauth_state: GoogleOAuth,
    oauth_pages: OAuthPages,
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

impl GAuthContainer for GlobalState {
    fn get_gauth_state(&self) -> &GoogleOAuth {
        &self.gauth_state
    }
}

impl OAuthPagesContainer for GlobalState {
    fn get_oauth_pages(&self) -> &OAuthPages {
        &self.oauth_pages
    }
}

impl OAuthStateContainer for GlobalState {
    fn get_oauth_state(&self) -> &OAuthState {
        &self.oauth_state
    }
}

async fn login(State(gauth): State<GoogleOAuth>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|ws| async move {
        let (auth_url, fut) = gauth.initiate_auth(["openid", "email"]);
        let Some(ws) = ws.easy_send(Message::Text(auth_url.to_string())).await else { return };

        let mut ws = PolledWebSocket::new(ws);
        let opt = select! {
            opt = fut => { opt }
            () = ws.wait_for_failure() => { return }
        };
        let Some(ws) = ws.into_inner().await else { return };

        if let Some(token) = opt {
            println!("{token:?}");
            ws.final_send_close_frame(1000, "Auth Succeeded!").await;
            return;
        };

        ws.final_send_close_frame(1000, "Auth Failed").await;
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
        .region(Region::new("us-west-1"))
        // .credentials_cache(CredentialsCache::lazy())
        .build();

    #[cfg(not(debug_assertions))]
    let aws_config = aws_config::load_from_env().await;

    let oauth_state = OAuthState::default();

    let state = GlobalState {
        db: DB::new(&aws_config),
        user_auth: UserAuth::new(&aws_config, config.cognito_client_id),
        gauth_state: new_google_oauth_from_file(
            config.google_client_secret_path,
            oauth_state.clone(),
        )
        .context("parsing google oauth")?,
        oauth_pages: OAuthPages::new(OAuthPagesSrc {
            internal_error: "Internal Error".into(),
            late: "Late".into(),
            invalid: "Invalid".into(),
            success: "Success".into(),
        }),
        oauth_state,
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
        ["^/oauth/"],
        [
            ("/oauth/redirect", oauth_redirect!()),
            // ("/sign_up", post(sign_up)),
            ("/login", get(login)),
        ],
    )
    .await
}
