use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};

use log::warn;
use parking_lot::Mutex;

use anyhow::Result;
use axum::{
    extract::{FromRef, Query, State},
    response::Html,
};
use oauth2::{
    basic::{BasicClient, BasicTokenType},
    reqwest::async_http_client,
    url::Url,
    AuthUrl, AuthorizationCode, ClientId, ClientSecret, CsrfToken, EmptyExtraTokenFields,
    PkceCodeChallenge, PkceCodeVerifier, RedirectUrl, RevocationUrl, Scope, StandardTokenResponse,
    TokenUrl,
};
use serde::Deserialize;
use tokio::{
    sync::oneshot::{channel, Sender},
    time::sleep,
};

use crate::log_targets;

// pub const GOOGLE_PROFILE_SCOPES: [&str; 2] = [
//     "https://www.googleapis.com/auth/userinfo.email",
//     "https://www.googleapis.com/auth/userinfo.profile",
// ];
// pub const GITHUB_PROFILE_SCOPES: [&str; 1] = ["user:email"];

/// How much time to wait for authorization to be granted by OAuth
pub const MAX_AUTH_WAIT_TIME: Duration = Duration::from_secs(180);
pub use oauth2::TokenResponse;

fn new_oauth_client(
    auth_url: String,
    token_url: String,
    client_id: String,
    client_secret: String,
    redirect_url: String,
    revocation_url: Option<String>,
) -> BasicClient {
    let auth_url = AuthUrl::new(auth_url).expect("Invalid authorization endpoint URL");
    let token_url = TokenUrl::new(token_url).expect("Invalid token endpoint URL");

    // Set up the config for the Google OAuth2 process.
    let mut client = BasicClient::new(
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
        auth_url,
        Some(token_url),
    )
    .set_redirect_uri(RedirectUrl::new(redirect_url).expect("Invalid redirect URL"));

    if let Some(revocation_url) = revocation_url {
        client = client.set_revocation_uri(
            RevocationUrl::new(revocation_url).expect("Invalid revocation endpoint URL"),
        );
    }

    client
}

#[derive(Clone)]
pub struct OAuth<const PKCE: bool> {
    oauth_state: OAuthState,
    client: Arc<BasicClient>,
}

impl<const PKCE: bool> OAuth<PKCE> {
    fn new(
        auth_url: String,
        token_url: String,
        client_id: String,
        client_secret: String,
        revocation_url: Option<String>,
        redirect_url: String,
        oauth_state: OAuthState,
    ) -> Self {
        Self {
            oauth_state,
            client: Arc::new(new_oauth_client(
                auth_url,
                token_url,
                client_id,
                client_secret,
                redirect_url,
                revocation_url,
            )),
        }
    }

    /// Initiates an OAuth attempt with the given scopes
    ///
    /// Returns a tuple with the authorization Url to give to the user, and a
    /// future that resolves to Some(token) where token is the OAuth token, or
    /// None if authentication timed out or failed
    pub fn initiate_auth(
        &self,
        scopes: impl IntoIterator<Item = impl Into<String>>,
    ) -> (Url, impl Future<Output = Option<OAuthToken>>) {
        let mut auth_request = self.client.authorize_url(CsrfToken::new_random);

        for scope in scopes {
            auth_request = auth_request.add_scope(Scope::new(scope.into()));
        }

        let pkce_code_verifier;

        // Generate the authorization URL to which we'll redirect the user.
        let (authorize_url, csrf_token) = if PKCE {
            let (pkce_code_challenge, tmp) = PkceCodeChallenge::new_random_sha256();
            pkce_code_verifier = Some(tmp);
            auth_request.set_pkce_challenge(pkce_code_challenge)
        } else {
            pkce_code_verifier = None;
            auth_request
        }
        .url();

        let (ready_sender, receiver) = channel();

        let oauth_state = self.oauth_state.clone();
        let untracker = oauth_state.track_session(
            csrf_token,
            pkce_code_verifier,
            self.client.clone(),
            ready_sender,
        );

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

pub type OAuthToken = StandardTokenResponse<EmptyExtraTokenFields, BasicTokenType>;

struct PendingSession {
    ready_sender: Sender<OAuthToken>,
    pkce_code_verifier: Option<PkceCodeVerifier>,
    client: Arc<BasicClient>,
}

#[derive(Default, Clone)]
pub struct OAuthState {
    pending_auths: Arc<Mutex<HashMap<String, PendingSession>>>,
}

struct Untracker {
    oauth_state: OAuthState,
    csrf_token: CsrfToken,
}

impl Drop for Untracker {
    fn drop(&mut self) {
        self.oauth_state.untrack_session(&self.csrf_token);
    }
}

impl OAuthState {
    fn track_session(
        &self,
        csrf_token: CsrfToken,
        pkce_code_verifier: Option<PkceCodeVerifier>,
        client: Arc<BasicClient>,
        ready_sender: Sender<OAuthToken>,
    ) -> Untracker {
        self.pending_auths.lock().insert(
            csrf_token.secret().clone(),
            PendingSession {
                ready_sender,
                pkce_code_verifier,
                client,
            },
        );

        Untracker {
            oauth_state: self.clone(),
            csrf_token,
        }
    }

    fn untrack_session(&self, csrf_token: &CsrfToken) {
        let _ = self.pending_auths.lock().remove(csrf_token.secret());
    }

    async fn verify_auth(
        &self,
        auth_code: AuthorizationCode,
        csrf_token: CsrfToken,
        pages: AuthPages,
    ) -> Html<String> {
        let pending = if let Some(x) = self.pending_auths.lock().remove(csrf_token.secret()) {
            x
        } else {
            return Html(pages.late.into_owned());
        };

        let client = pending.client;

        let mut request = client.exchange_code(auth_code);

        if let Some(pkce_code_verifier) = pending.pkce_code_verifier {
            request = request.set_pkce_verifier(pkce_code_verifier);
        }

        let token = match request.request_async(async_http_client).await {
            Ok(x) => x,
            Err(e) => {
                return match e {
                    oauth2::RequestTokenError::ServerResponse(x) => {
                        // TODO Provide more info
                        warn!(
                            target: log_targets::SUSPICIOUS_SECURITY,
                            "Received bad gauth response: {x:?}"
                        );
                        Html(pages.invalid.into_owned())
                    }
                    _ => Html(pages.internal_error.into_owned()),
                };
            }
        };

        let _ = pending.ready_sender.send(token);
        Html(pages.success.into_owned())
    }
}

#[derive(Deserialize)]
pub struct AuthRedirectParams {
    state: String,
    code: String,
}

pub trait OAuthStateContainer {
    fn get_oauth_state(&self) -> &OAuthState;
}

impl<T: OAuthStateContainer> FromRef<T> for OAuthState {
    fn from_ref(input: &T) -> Self {
        input.get_oauth_state().clone()
    }
}

pub async fn oauth_redirect_handler(
    Query(AuthRedirectParams { state, code }): Query<AuthRedirectParams>,
    State(oauth_state): State<OAuthState>,
    State(pages): State<AuthPages>,
) -> Html<String> {
    oauth_state
        .verify_auth(AuthorizationCode::new(code), CsrfToken::new(state), pages)
        .await
}

#[macro_export]
macro_rules! oauth_redirect {
    () => {
        $crate::axum::routing::get($crate::auth::oauth2::oauth_redirect_handler)
    };
}

pub use oauth_redirect;

use super::auth_pages::AuthPages;

pub mod google {
    use std::{fs::read_to_string, path::Path};

    use serde_json::from_str;

    use super::*;
    #[derive(Clone)]
    pub struct GoogleOAuth(pub OAuth<true>);

    pub trait GAuthContainer {
        fn get_gauth(&self) -> &GoogleOAuth;
    }

    impl<T: GAuthContainer> FromRef<T> for GoogleOAuth {
        fn from_ref(input: &T) -> Self {
            input.get_gauth().clone()
        }
    }

    pub fn new_google_oauth_from_file(
        filename: impl AsRef<Path>,
        redirect_url: impl Into<String>,
        oauth_state: OAuthState,
    ) -> Result<GoogleOAuth> {
        #[derive(Deserialize, Debug)]
        struct ClientSecret {
            client_id: String,
            client_secret: String,
            auth_uri: String,
            token_uri: String,
        }
        #[derive(Deserialize)]
        struct WebSecret {
            installed: ClientSecret,
        }

        let secrets: WebSecret = from_str(&read_to_string(filename)?)?;
        let secrets = secrets.installed;

        Ok(GoogleOAuth(OAuth::new(
            secrets.auth_uri,
            secrets.token_uri,
            secrets.client_id,
            secrets.client_secret,
            Some("https://oauth2.googleapis.com/revoke".to_string()),
            redirect_url.into(),
            oauth_state,
        )))
    }
}

pub mod github {
    use std::{fs::read_to_string, path::Path};

    use serde_json::from_str;

    use super::*;
    #[derive(Clone)]
    pub struct GithubOAuth(pub OAuth<false>);

    pub trait GithubOAuthContainer {
        fn get_github_auth_state(&self) -> &GithubOAuth;
    }

    impl<T: GithubOAuthContainer> FromRef<T> for GithubOAuth {
        fn from_ref(input: &T) -> Self {
            input.get_github_auth_state().clone()
        }
    }

    pub fn new_github_oauth_from_file(
        filename: impl AsRef<Path>,
        redirect_url: impl Into<String>,
        oauth_state: OAuthState,
    ) -> Result<GithubOAuth> {
        #[derive(Deserialize, Debug)]
        struct ClientSecret {
            client_id: String,
            client_secret: String,
        }

        let secrets: ClientSecret = from_str(&read_to_string(filename)?)?;

        Ok(GithubOAuth(OAuth::new(
            "https://github.com/login/oauth/authorize".to_string(),
            "https://github.com/login/oauth/access_token".to_string(),
            secrets.client_id,
            secrets.client_secret,
            None,
            redirect_url.into(),
            oauth_state,
        )))
    }
}
