use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use axum::{
    extract::{FromRef, Query, State},
    response::Html,
};
use openid::{error::ClientError, DiscoveredClient, Options};
use parking_lot::Mutex;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use reqwest::Url;

pub use openid::Userinfo;

/// How much time to wait for authentication to be granted by OpenID
const MAX_AUTH_WAIT_TIME: Duration = Duration::from_secs(180);
const CSRF_TOKEN_SIZE: usize = 32;

async fn new_oidc_client(
    client_id: String,
    client_secret: String,
    redirect_url: String,
    issuer_url: Url,
) -> Result<DiscoveredClient, openid::error::Error> {
    DiscoveredClient::discover(client_id, client_secret, Some(redirect_url), issuer_url).await
}

#[derive(Clone)]
pub struct OIDC {
    oidc_state: OIDCState,
    client: Arc<DiscoveredClient>,
}

impl OIDC {
    async fn new(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer_url: Url,
        oidc_state: OIDCState,
    ) -> Result<Self, openid::error::Error> {
        Ok(Self {
            oidc_state,
            client: Arc::new(
                new_oidc_client(client_id, client_secret, redirect_url, issuer_url).await?,
            ),
        })
    }

    /// Initiates an OAuth attempt with the given scopes
    ///
    /// Returns a tuple with the authorization Url to give to the user, and a
    /// future that resolves to Some(token) where token is the OAuth token, or
    /// None if authentication timed out or failed
    pub fn initiate_auth(
        &self,
        scopes: impl IntoIterator<Item = impl AsRef<str>>,
    ) -> (Url, impl Future<Output = Option<Userinfo>>) {
        let csrf_token: String = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(CSRF_TOKEN_SIZE)
            .map(char::from)
            .collect();

        let mut scope_str = String::new();

        for scope in scopes {
            scope_str += scope.as_ref();
            scope_str += " ";
        }
        scope_str.pop();

        let options = Options {
            scope: Some(scope_str),
            state: Some(csrf_token.clone()),
            ..Default::default()
        };
        let authorize_url = self.client.auth_url(&options);

        let (ready_sender, receiver) = channel();

        let oidc_state = self.oidc_state.clone();
        let untracker = oidc_state.track_session(csrf_token, self.client.clone(), ready_sender);

        let fut = async move {
            let _untracker = untracker;

            tokio::select! {
                res = receiver => {
                    res.ok()
                },
                () = sleep(MAX_AUTH_WAIT_TIME) => {
                    None
                }
            }
        };

        (authorize_url, fut)
    }
}

struct PendingSession {
    ready_sender: Sender<Userinfo>,
    client: Arc<DiscoveredClient>,
}

#[derive(Default, Clone)]
pub struct OIDCState {
    pending_auths: Arc<Mutex<HashMap<String, PendingSession>>>,
}

struct Untracker {
    oauth_state: OIDCState,
    csrf_token: String,
}

impl Drop for Untracker {
    fn drop(&mut self) {
        self.oauth_state.untrack_session(&self.csrf_token);
    }
}

impl OIDCState {
    fn track_session(
        &self,
        csrf_token: String,
        client: Arc<DiscoveredClient>,
        ready_sender: Sender<Userinfo>,
    ) -> Untracker {
        self.pending_auths.lock().insert(
            csrf_token.clone(),
            PendingSession {
                ready_sender,
                client,
            },
        );

        Untracker {
            oauth_state: self.clone(),
            csrf_token,
        }
    }

    fn untrack_session(&self, csrf_token: &str) {
        let _ = self.pending_auths.lock().remove(csrf_token);
    }

    async fn verify_auth(
        &self,
        auth_code: String,
        csrf_token: String,
        pages: AuthPages,
    ) -> Html<String> {
        let pending = if let Some(x) = self.pending_auths.lock().remove(&csrf_token) {
            x
        } else {
            return Html(pages.late.into_owned());
        };

        let client = pending.client;

        let mut token = match client.request_token(&auth_code).await {
            Ok(x) => match openid::Token::from(x).id_token {
                Some(x) => x,
                None => return Html(pages.internal_error.into_owned())
            }
            Err(e) => {
                return match e {
                    ClientError::OAuth2(e) => match e.error {
                        openid::OAuth2ErrorCode::InvalidGrant => Html(pages.invalid.into_owned()),
                        _ => Html(pages.internal_error.into_owned()),
                    },
                    _ => Html(pages.internal_error.into_owned()),
                }
            }
        };

        if let Err(_) = client.decode_token(&mut token) {
            return Html(pages.internal_error.into_owned())
        }
        if let Err(_) = client.validate_token(&token, None, None) {
            return Html(pages.invalid.into_owned())
        }
        let userinfo = token.payload().unwrap().userinfo.clone();

        let _ = pending.ready_sender.send(userinfo);
        Html(pages.success.into_owned())
    }
}

#[derive(Deserialize)]
pub struct AuthRedirectParams {
    state: String,
    code: String,
}

pub trait OIDCStateContainer {
    fn get_oidc_state(&self) -> &OIDCState;
}

impl<T: OIDCStateContainer> FromRef<T> for OIDCState {
    fn from_ref(input: &T) -> Self {
        input.get_oidc_state().clone()
    }
}

pub async fn oidc_redirect_handler(
    Query(AuthRedirectParams { state, code }): Query<AuthRedirectParams>,
    State(oauth_state): State<OIDCState>,
    State(pages): State<AuthPages>,
) -> Html<String> {
    oauth_state.verify_auth(code, state, pages).await
}

#[macro_export]
macro_rules! openid_redirect {
    () => {
        $crate::axum::routing::get($crate::auth::openid::oidc_redirect_handler)
    };
}

pub use openid_redirect;
use serde::Deserialize;
use tokio::{
    sync::oneshot::{channel, Sender},
    time::sleep,
};

use super::auth_pages::AuthPages;

pub mod google {
    use std::{fs::read_to_string, path::Path};

    use serde_json::from_str;

    use super::*;
    #[derive(Clone)]
    pub struct GoogleOIDC(pub OIDC);

    pub trait GOIDCContainer {
        fn get_goidc(&self) -> &GoogleOIDC;
    }

    impl<T: GOIDCContainer> FromRef<T> for GoogleOIDC {
        fn from_ref(input: &T) -> Self {
            input.get_goidc().clone()
        }
    }

    pub async fn new_google_oidc_from_file(
        filename: impl AsRef<Path>,
        oidc_state: OIDCState,
        redirect_url: &str,
    ) -> anyhow::Result<GoogleOIDC> {
        #[derive(Deserialize, Debug)]
        struct ClientSecret {
            client_id: String,
            client_secret: String,
        }
        #[derive(Deserialize)]
        struct WebSecret {
            web: ClientSecret,
        }

        let secrets: WebSecret = from_str(&read_to_string(filename)?)?;
        let secrets = secrets.web;

        Ok(GoogleOIDC(
            OIDC::new(
                secrets.client_id,
                secrets.client_secret,
                redirect_url.into(),
                Url::parse("https://accounts.google.com").expect("URL to be valid"),
                oidc_state,
            )
            .await?,
        ))
    }
}
