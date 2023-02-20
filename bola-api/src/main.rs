use std::{ops::Deref, time::Duration};

// use aws_config::{SdkConfig};
use db::{DB, UserProfile};
use mangle_api_core::{
    anyhow::{self, Context},
    auth::{
        auth_pages::{AuthPages, AuthPagesContainer, AuthPagesSrc},
        openid::{
            google::{new_google_oidc_from_file, GOIDCContainer, GoogleOIDC},
            OIDCState, OIDCStateContainer,
        }, token::{VerifiedToken, HeaderTokenGranter},
    },
    axum::{
        extract::{ws::Message, FromRef, State, WebSocketUpgrade},
        http::{HeaderValue, StatusCode},
        response::Response,
        routing::get,
    },
    get_pipe_name, make_app, openid_redirect, pre_matches, start_api,
    tokio::{self, select},
    ws::{ManagedWebSocket, WebSocketCode},
    // ws::{PolledWebSocket, WsExt, ManagedWebSocket, WebSocketCode},
    BaseConfig, BindAddress, serde_json, log::error, create_header_token_granter,
};

mod config;
mod db;
mod leaderboard;

use config::Config;
use rustrict::CensorStr;


create_header_token_granter!( LoginTokenGranter "LoginToken" 32 String);


#[derive(Clone)]
struct GlobalState {
    db: DB,
    oidc_state: OIDCState,
    goidc: GoogleOIDC,
    auth_pages: AuthPages,
    login_tokens: LoginTokenGranter
}


impl FromRef<GlobalState> for LoginTokenGranter {
    fn from_ref(input: &GlobalState) -> Self {
        input.login_tokens.clone()
    }
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

async fn login(State(GoogleOIDC(oidc)): State<GoogleOIDC>, State(db): State<DB>, State(token_granter): State<LoginTokenGranter>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|ws| async move {
        let (auth_url, fut) = oidc.initiate_auth(["openid", "email"]);
        let ws = ManagedWebSocket::new(ws, Duration::from_secs(45));

        let Some(ws) = ws.send(auth_url.to_string()) else { return };
        let mut ws = Some(ws);

        let opt = select! {
            opt = fut => { opt }
            () = ManagedWebSocket::loop_recv(&mut ws, None) => { return }
        };
        let mut ws = ws.unwrap();

        let Some(data) = opt else {
            ws.close_frame(WebSocketCode::Ok, "Auth Failed");
            return
        };
        let Some(email) = data.email else {
            ws.close_frame(WebSocketCode::BadPayload, "Auth Failed");
            return
        };

        match db.get_user_profile_by_email(&email).await {
            Ok(Some(ref x)) => {
                let Some(ws_tmp) = ws.send(serde_json::to_string(x).unwrap()) else { return };
                let Some(ws_tmp) = ws_tmp.send(
                    token_granter.create_token(email).to_str().unwrap()
                ) else { return };
                ws = ws_tmp;
            }
            Ok(None) => {
                let Some(ws_tmp) = ws.send("Sign Up") else { return };
                ws = ws_tmp;
                let mut profile;

                loop {
                    let Some((ws_tmp, msg)) = ws.recv().await else { return };
                    ws = ws_tmp;

                    let Message::Text(msg) = msg else {
                        ws.close_frame(WebSocketCode::BadPayload, "Expected profile");
                        return
                    };

                    let Ok(tmp_profile) = serde_json::from_str::<UserProfile>(&msg) else {
                        ws.close_frame(WebSocketCode::BadPayload, "");
                        return
                    };
                    profile = tmp_profile;

                    if profile.username.is_inappropriate() {
                        let Some(ws_tmp) = ws.send("Inappropriate username") else {
                            return
                        };
                        ws = ws_tmp;
                        continue
                    }

                    match db.is_username_taken(&profile.username).await {
                        Ok(true) => {
                            let Some(ws_tmp) = ws.send("Username already used") else {
                                return
                            };
                            ws = ws_tmp;
                            continue
                        }
                        Ok(false) => {}
                        Err(e) => {
                            error!(target: "login", "{e:?}");
                            ws.close_frame(WebSocketCode::InternalError, "");
                            return
                        }
                    };

                    break
                }

                if let Err(e) = db.create_user_profile(email.clone(), profile).await {
                    error!(target: "login", "{e:?}");
                    ws.close_frame(WebSocketCode::InternalError, "");
                    return
                }

                let Some(ws_tmp) = ws.send(
                    token_granter.create_token(email).to_str().unwrap()
                ) else { return };
                ws = ws_tmp;
            }
            Err(e) => {
                error!(target: "login", "{e:?}");
                ws.close_frame(WebSocketCode::InternalError, "");
                return
            }
        };

        ws.close_frame(WebSocketCode::Ok, "");
    })
}


async fn quick_login(token: VerifiedToken<LoginTokenGranter>, State(db): State<DB>) -> (StatusCode, String) {
    match db.get_user_profile_by_email(token.item.deref().clone()).await {
        Ok(Some(ref x)) => (StatusCode::OK, serde_json::to_string(x).expect("Correct serialization of UserProfile")),
        Ok(None) => {
            (StatusCode::BAD_REQUEST, "User does not exist".into())
        }
        Err(e) => {
            error!(target: "quick_login", "{e:?}");
            (StatusCode::INTERNAL_SERVER_ERROR, String::new())
        }
    }
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

    let state = GlobalState {
        db: DB::new(&aws_config, config.bola_profiles_table),
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
        login_tokens: LoginTokenGranter::new(Duration::from_secs(config.token_duration as u64))
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
        [
            "^/oidc/"
        ],
        [
            ("/oidc/redirect", openid_redirect!()),
            // ("/sign_up", post(sign_up)),
            ("/login", get(login)),
            ("/quick_login", get(quick_login)),
        ],
    )
    .await
}
