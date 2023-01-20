use std::{sync::{Arc, Weak}, collections::HashMap, time::{Duration}};

use log::warn;
use parking_lot::Mutex;

use axum::{extract::{Query, State, WebSocketUpgrade, ws::Message, FromRef}, response::Response};
use oauth2::{url::Url, AuthorizationCode, CsrfToken, AuthUrl, TokenUrl, basic::BasicClient, ClientId, ClientSecret, RedirectUrl, RevocationUrl, PkceCodeChallenge, Scope, reqwest::async_http_client, PkceCodeVerifier};
use anyhow::{Result};
use serde::Deserialize;
use tokio::{sync::oneshot::{channel, Sender}, time::sleep};
use axum::response::Html;

use crate::{log_targets};


pub const GOOGLE_PROFILE_SCOPES: [&str; 2] = [
    "https://www.googleapis.com/auth/userinfo.email",
    "https://www.googleapis.com/auth/userinfo.profile",
];
pub const GITHUB_PROFILE_SCOPES: [&str; 1] = [
    "user:email",
];

/// How much time to wait for authorization to be granted by OAuth
pub const MAX_AUTH_WAIT_TIME: Duration = Duration::from_secs(180);


fn new_oauth_client(
    auth_url: String,
    token_url: String,
    client_id: String,
    client_secret: String,
    redirect_url: String,
    revocation_url: Option<String>
) -> BasicClient {
    let auth_url = AuthUrl::new(auth_url)
        .expect("Invalid authorization endpoint URL");
    let token_url = TokenUrl::new(token_url)
        .expect("Invalid token endpoint URL");

    // Set up the config for the Google OAuth2 process.
    let mut client = BasicClient::new(
        ClientId::new(client_id),
        Some(ClientSecret::new(client_secret)),
        auth_url,
        Some(token_url),
    )
    .set_redirect_uri(
        RedirectUrl::new(redirect_url)
            .expect("Invalid redirect URL"),
    );

    if let Some(revocation_url) = revocation_url {
        client = client.set_revocation_uri(
            RevocationUrl::new(revocation_url)
                .expect("Invalid revocation endpoint URL"),
        );
    }
    
    client
}


struct AuthSession {
    pkce_code_verifier: Option<PkceCodeVerifier>,
    // creation_time: Instant,
    client: Weak<BasicClient>
}


struct InitiatedAuth {
    url: Url,
    session: AuthSession,
    csrf_token: CsrfToken
}


#[derive(Clone)]
pub struct OAuth<const PKCE: bool> {
    client: Arc<BasicClient>
}


impl<const PKCE: bool> OAuth<PKCE> {
    fn new(
        auth_url: String,
        token_url: String,
        client_id: String,
        client_secret: String,
        revocation_url: Option<String>
    ) -> OAuth<PKCE> {
        Self {
            client: Arc::new(new_oauth_client(
                auth_url,
                token_url,
                client_id,
                client_secret,
                "http://localhost/oauth/redirect".to_string(),
                revocation_url
            ))
        }
    }

    fn initiate_auth(&self, scopes: impl IntoIterator<Item=impl Into<String>>) -> InitiatedAuth {
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
        }.url();
        
        let session = AuthSession {
            pkce_code_verifier,
            // creation_time: Instant::now(),
            client: Arc::downgrade(&self.client)
        };

        InitiatedAuth {
            session,
            url: authorize_url,
            csrf_token
        }
    }
}


struct PendingSession {
    session: AuthSession,
    ready_sender: Sender<()>
}


#[derive(Default, Clone)]
pub struct OAuthState {
    pending_auths: Arc<Mutex<HashMap<String, PendingSession>>>
}


impl OAuthState {
    fn track_session(&self, csrf_token: &CsrfToken, session: AuthSession, ready_sender: Sender<()>) {
        self.pending_auths.lock().insert(
            csrf_token.secret().clone(),
            PendingSession {
                session,
                ready_sender
            }
        );
    }

    fn untrack_session(&self, csrf_token: &CsrfToken) {
        let _ = self.pending_auths.lock().remove(csrf_token.secret());
    }

    async fn verify_auth(&self, auth_code: AuthorizationCode, csrf_token: CsrfToken) -> Html<&'static str> {
        let pending = if let Some(x) = self.pending_auths.lock().remove(csrf_token.secret()) {
            x
        } else {
            return Html(include_str!("../static/late_auth.html"))
        };

        let client = match pending
            .session
            .client
            .upgrade() {
                Some(x) => x,
                None => return Html(include_str!("../static/internal_error.html"))
            };
        
        let mut request = client.exchange_code(auth_code);

        if let Some(pkce_code_verifier) = pending.session.pkce_code_verifier {
            request = request.set_pkce_verifier(pkce_code_verifier);
        }

        let token = match request
            .request_async(async_http_client)
            .await {
                Ok(x) => x,
                Err(e) => return match e {
                    oauth2::RequestTokenError::ServerResponse(x) => {
                        // TODO Provide more info
                        warn!(target: log_targets::SUSPICIOUS_SECURITY, "Received bad gauth response: {x:?}");
                        Html(include_str!("../static/invalid_auth.html"))
                    }
                    _ => Html(include_str!("../static/internal_error.html"))
                }
            };

        let _ = pending.ready_sender.send(());
        Html(include_str!("../static/successful_auth.html"))
    }
}


#[derive(Deserialize)]
pub struct AuthRedirectParams {
    state: String,
    code: String
}


pub trait OAuthStateContainer {
    fn get_oauth_state(&self) -> &OAuthState;
}


impl<T: OAuthStateContainer> FromRef<T> for OAuthState {
    fn from_ref(input: &T) -> Self {
        input.get_oauth_state().clone()
    }
}


#[axum::debug_handler]
pub async fn oauth_redirect(
    Query(AuthRedirectParams { state, code }): Query<AuthRedirectParams>,
    State(oauth_state): State<OAuthState>
) -> Html<&'static str> {
    oauth_state.verify_auth(
        AuthorizationCode::new(code),
        CsrfToken::new(state)
    ).await
}


pub fn initiate_oauth<const PKCE: bool>(
    ws: WebSocketUpgrade,
    oauth_state: OAuthState,
    client: OAuth<PKCE>,
    scopes: impl IntoIterator<Item=impl Into<String>> + Send + 'static
) -> Response {
    ws.on_upgrade(async move |mut ws| {
        let (sender, receiver) = channel();
        
        let init = client.initiate_auth(scopes);
        let url = init.url;

        oauth_state.track_session(
            &init.csrf_token,
            init.session,
            sender
        );

        if ws.send(Message::Text(url.to_string())).await.is_err() {
            return
        }

        tokio::select! {
            () = async {
                loop {
                    if ws.recv().await.is_none() {
                        break
                    }
                }
            } => { }

            () = sleep(MAX_AUTH_WAIT_TIME) => { }
            
            res = receiver => if let Ok(_x) = res {
                let _ = ws.send(Message::Text("authed".into())).await;
            } else { }
        }

        oauth_state.untrack_session(&init.csrf_token);
    })
}


pub mod google {
    use std::{path::Path, fs::read_to_string};

    use serde_json::from_str;

    use super::*;
    pub type GoogleOAuth = OAuth<true>;

    pub trait GAuthContainer {
        fn get_gauth_state(&self) -> &GoogleOAuth;
    }
    
    impl<T: GAuthContainer> FromRef<T> for GoogleOAuth {
        fn from_ref(input: &T) -> Self {
            input.get_gauth_state().clone()
        }
    }

    pub fn new_google_oauth_from_file(
        filename: impl AsRef<Path>
    ) -> Result<GoogleOAuth> {
        #[derive(Deserialize, Debug)]
        struct ClientSecret {
            client_id: String,
            client_secret: String,
            auth_uri: String,
            token_uri: String
        }
        #[derive(Deserialize)]
        struct WebSecret {
            web: ClientSecret
        }

        let secrets: WebSecret = from_str(&read_to_string(filename)?)?;
        let secrets = secrets.web;

        Ok(
            GoogleOAuth::new(
                secrets.auth_uri,
                secrets.token_uri,
                secrets.client_id,
                secrets.client_secret,
                Some("https://oauth2.googleapis.com/revoke".to_string())
            )
        )
    }

    // #[axum::debug_handler]
    // pub async fn initiate_google_auth(
    //     ws: WebSocketUpgrade,
    //     State(oauth_state): State<OAuthState>,
    //     State(client): State<GoogleOAuth>
    // ) -> Response {
    //     initiate_oauth(
    //         ws,
    //         oauth_state,
    //         client,
    //         GOOGLE_PROFILE_SCOPES
    //     )
    // }
}


pub mod github {
    use std::{path::Path, fs::read_to_string};

    use serde_json::from_str;

    use super::*;
    pub type GithubOAuth = OAuth<false>;

    pub trait GithubOAuthContainer {
        fn get_github_auth_state(&self) -> &GithubOAuth;
    }
    
    impl<T: GithubOAuthContainer> FromRef<T> for GithubOAuth {
        fn from_ref(input: &T) -> Self {
            input.get_github_auth_state().clone()
        }
    }

    pub fn new_github_oauth_from_file(
        filename: impl AsRef<Path>
    ) -> Result<GithubOAuth> {
        #[derive(Deserialize, Debug)]
        struct ClientSecret {
            client_id: String,
            client_secret: String,
        }

        let secrets: ClientSecret = from_str(&read_to_string(filename)?)?;

        Ok(
            GithubOAuth::new(
                "https://github.com/login/oauth/authorize".to_string(),
                "https://github.com/login/oauth/access_token".to_string(),
                secrets.client_id,
                secrets.client_secret,
                None
            )
        )
    }
}
