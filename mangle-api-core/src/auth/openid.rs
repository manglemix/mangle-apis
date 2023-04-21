use std::{collections::HashMap, future::Future, sync::Arc, time::Duration, ops::Deref};

use axum::{
    body::HttpBody,
    extract::{FromRef, Query, State},
    response::Html,
    routing::MethodRouter,
};
use log::error;
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
pub struct OIDC<S: Deref<Target=OIDCState> + Clone> {
    oidc_state: S,
    client: Arc<DiscoveredClient>,
}


impl<S: Deref<Target=OIDCState> + Clone> OIDC<S> {
    async fn new(
        client_id: String,
        client_secret: String,
        redirect_url: String,
        issuer_url: Url,
        oidc_state: S,
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

        let untracker = track_session(self.oidc_state.clone(), csrf_token, self.client.clone(), ready_sender);

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

#[derive(Default)]
pub struct OIDCState {
    pending_auths: Mutex<HashMap<String, PendingSession>>,
}

struct Untracker<S: Deref<Target=OIDCState>> {
    oauth_state: S,
    csrf_token: String,
}

impl<S: Deref<Target=OIDCState>> Drop for Untracker<S> {
    fn drop(&mut self) {
        self.oauth_state.untrack_session(&self.csrf_token);
    }
}


fn track_session<S: Deref<Target=OIDCState>>(
    oauth_state: S,
    csrf_token: String,
    client: Arc<DiscoveredClient>,
    ready_sender: Sender<Userinfo>,
) -> Untracker<S> {
    oauth_state.pending_auths.lock().insert(
        csrf_token.clone(),
        PendingSession {
            ready_sender,
            client,
        },
    );

    Untracker {
        oauth_state,
        csrf_token,
    }
}


impl OIDCState {
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
                None => return Html(pages.internal_error.into_owned()),
            },
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

        if let Err(e) = client.decode_token(&mut token) {
            let e = anyhow::Error::from(e);
            error!(target: "openid", "{:?}", e.context("decoding openid token"));
            return Html(pages.internal_error.into_owned());
        }
        if let Err(e) = client.validate_token(&token, None, None) {
            let e = anyhow::Error::from(e);
            error!(target: "openid", "{:?}", e.context("validating openid token"));
            return Html(pages.invalid.into_owned());
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

pub async fn oidc_redirect_handler<S>(
    Query(AuthRedirectParams { state, code }): Query<AuthRedirectParams>,
    State(global_state): State<S>,
    State(pages): State<AuthPages>
) -> Html<String>
where
    S: AsRef<OIDCState>
{
    AsRef::<OIDCState>::as_ref(&global_state).verify_auth(code, state, pages).await
}

pub fn openid_redirect<S, B>() -> MethodRouter<S, B>
where
    AuthPages: FromRef<S>,
    S: AsRef<OIDCState> + Send + Sync + Clone + 'static,
    B: Send + Sync + HttpBody + 'static, // AuthPages: FromRef<S>
{
    axum::routing::get(oidc_redirect_handler::<S>)
}

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
    pub struct GoogleOIDC<S: Deref<Target=OIDCState> + Clone>(pub OIDC<S>);

    pub async fn new_google_oidc_from_file<S: Deref<Target=OIDCState> + Clone>(
        filename: impl AsRef<Path>,
        oidc_state: S,
        redirect_url: &str,
    ) -> anyhow::Result<GoogleOIDC<S>> {
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
