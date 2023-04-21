use axum::extract::FromRef;
use mangle_api_core::{
    auth::{
        auth_pages::AuthPages,
        openid::{google::GoogleOIDC, OIDCState},
    },
    neo_api::NeoApiConfig,
};

use crate::{
    db::DB, leaderboard::Leaderboard, multiplayer::Multiplayer, tournament::Tournament,
    ws_api::WsApiHandler, LoginTokenGranter,
};

#[derive(Clone, Copy)]
pub(crate) struct GlobalState {
    pub db: &'static DB,
    pub oidc_state: &'static OIDCState,
    pub goidc: &'static GoogleOIDC<&'static OIDCState>,
    pub auth_pages: &'static AuthPages,
    pub login_tokens: &'static LoginTokenGranter,
    pub leaderboard: &'static Leaderboard,
    pub ws_api: &'static NeoApiConfig<WsApiHandler>,
    pub tournament: &'static Tournament,
    pub multiplayer: &'static Multiplayer,
}

impl AsRef<LoginTokenGranter> for GlobalState {
    fn as_ref(&self) -> &LoginTokenGranter {
        self.login_tokens
    }
}

impl AsRef<NeoApiConfig<WsApiHandler>> for GlobalState {
    fn as_ref(&self) -> &NeoApiConfig<WsApiHandler> {
        self.ws_api
    }
}

impl AsRef<OIDCState> for GlobalState {
    fn as_ref(&self) -> &OIDCState {
        self.oidc_state
    }
}

impl FromRef<GlobalState> for AuthPages {
    fn from_ref(input: &GlobalState) -> Self {
        input.auth_pages.clone()
    }
}

#[macro_export]
macro_rules! new_global {
    ($config:expr, $https_identity:expr, $aws_config:expr) => {{
        use std::fs::read_to_string;

        let internal_error_page = read_to_string(&$config.internal_error_path)
            .context(format!("Reading {}", $config.internal_error_path))?;
        let invalid_page = read_to_string(&$config.invalid_path)
            .context(format!("Reading {}", $config.invalid_path))?;
        let success_page = read_to_string(&$config.success_path)
            .context(format!("Reading {}", $config.success_path))?;
        let late_page =
            read_to_string(&$config.late_path).context(format!("Reading {}", $config.late_path))?;
        let auth_pages = manglext::immut_leak(mangle_api_core::auth::auth_pages::AuthPages::new(
            mangle_api_core::auth::auth_pages::AuthPagesSrc {
                internal_error: internal_error_page,
                late: late_page,
                invalid: invalid_page,
                success: success_page,
            },
        ));

        let oidc_state = manglext::immut_leak(mangle_api_core::auth::openid::OIDCState::default());

        let node = manglext::immut_leak(
            mangle_api_core::distributed::Node::new(
                $config.sibling_domains,
                $config.network_port,
                $https_identity.clone(),
                $crate::network::SiblingNetworkHandler::new(),
            )
            .await?,
        );
        let db = manglext::immut_leak($crate::db::DB::new(
            &$aws_config,
            $config.bola_profiles_table,
        ));

        let goidc = manglext::immut_leak(
            mangle_api_core::auth::openid::google::new_google_oidc_from_file(
                $config.google_client_secret_path,
                oidc_state.clone(),
                &($config.oidc_redirect_base + "/oidc/redirect"),
            )
            .await
            .context("parsing google oauth")?,
        );

        let leaderboard =
            manglext::immut_leak($crate::leaderboard::Leaderboard::new(db.clone(), node, 5).await?);
        let login_tokens = manglext::immut_leak(LoginTokenGranter::new($config.token_duration));
        let ws_api = manglext::immut_leak(mangle_api_core::neo_api::NeoApiConfig::new(
            WS_PING_DELAY,
            $crate::ws_api::WsApiHandler::new(leaderboard, db, &goidc.0, login_tokens),
        ));

        $crate::state::GlobalState {
            goidc,
            auth_pages,
            oidc_state,
            login_tokens,
            leaderboard,
            db,
            // api_conn_manager: APIConnectionManager::new(WS_PING_DELAY),
            tournament: manglext::immut_leak($crate::tournament::Tournament::new(
                $config.start_week_time,
            )),
            multiplayer: manglext::immut_leak($crate::multiplayer::Multiplayer::default()),
            ws_api,
        }
    }};
}
