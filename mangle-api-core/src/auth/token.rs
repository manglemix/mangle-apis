use std::{hash::Hash, sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{FromRequestParts},
    http::{request::Parts, HeaderValue, StatusCode},
    response::IntoResponse,
};
use bimap::BiMap;
use parking_lot::Mutex;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use tokio::{
    spawn,
    time::sleep, task::JoinHandle,
};

struct TokenEntry<ID> {
    _expiry_handle: JoinHandle<()>,
    identifier: Arc<ID>,
}

impl<ID: Hash> Hash for TokenEntry<ID> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.identifier.hash(state);
    }
}

impl<ID: PartialEq> PartialEq for TokenEntry<ID> {
    fn eq(&self, other: &Self) -> bool {
        self.identifier == other.identifier
    }
}

impl<ID: Eq> Eq for TokenEntry<ID> {}


impl<ID> Drop for TokenEntry<ID> {
    fn drop(&mut self) {
        self._expiry_handle.abort();
    }
}


pub struct TokenGranter<C: TokenConfig> {
    // When the sender gets dropped, the task responsible for expiring the token will complete
    tokens: Arc<Mutex<BiMap<HeaderValue, TokenEntry<C::TokenIdentifier>>>>,
    token_duration: Duration,
}

pub trait TokenConfig: Send + Sync + 'static {
    type TokenIdentifier: Send + Sync + Hash + Eq + 'static;
    const TOKEN_LENGTH: usize;
}

pub trait HeaderTokenConfig: TokenConfig {
    const HEADER_NAME: &'static str;
}


impl<C: TokenConfig> TokenGranter<C> {
    pub fn new(token_duration: Duration) -> Self {
        Self {
            tokens: Default::default(),
            token_duration
        }
    }

    pub fn create_token(&self, id: impl Into<Arc<C::TokenIdentifier>>) -> VerifiedToken<C> {
        let id = id.into();

        let bytes: Vec<u8> = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(C::TOKEN_LENGTH)
            .collect();

        let token = unsafe { HeaderValue::from_maybe_shared_unchecked(bytes) };
        let token2 = token.clone();

        let tokens = self.tokens.clone();
        let token_duration  = self.token_duration;

        let entry = TokenEntry {
            _expiry_handle: spawn(async move {
                sleep(token_duration).await;
                tokens.lock().remove_by_left(&token2);
            }),
            identifier: id.clone()
        };

        self.tokens.lock().insert(token.clone(), entry);

        VerifiedToken { token, identifier: id }
    }

    pub fn revoke_token(&self, token: &HeaderValue) {
        self.tokens.lock().remove_by_left(token);
    }

    pub fn verify_token(&self, token: &HeaderValue) -> Option<VerifiedToken<C>> {
        let mut lock = self.tokens.lock();
        let (token, entry) = lock.remove_by_left(token)?;
        let identifier = entry.identifier.clone();
        lock.insert(token.clone(), entry);
        Some(VerifiedToken { token, identifier })
    }
}

pub struct VerifiedToken<C: TokenConfig> {
    pub token: HeaderValue,
    pub identifier: Arc<C::TokenIdentifier>,
}

pub enum TokenVerificationError {
    MissingToken,
    InvalidTokenLength,
    InvalidToken,
}

impl IntoResponse for TokenVerificationError {
    fn into_response(self) -> axum::response::Response {
        match self {
            TokenVerificationError::MissingToken => (StatusCode::UNAUTHORIZED, "Missing token"),
            TokenVerificationError::InvalidToken => {
                (StatusCode::UNAUTHORIZED, "Invalid or expired token")
            }
            TokenVerificationError::InvalidTokenLength => {
                (StatusCode::UNAUTHORIZED, "Invalid length for token")
            }
        }
        .into_response()
    }
}

#[async_trait]
impl<S, C> FromRequestParts<S> for VerifiedToken<C>
where
    C: HeaderTokenConfig,
    S: AsRef<TokenGranter<C>> + Sync
{
    type Rejection = TokenVerificationError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(token) = parts.headers.get(C::HEADER_NAME) {
            if token.len() != C::TOKEN_LENGTH {
                return Err(TokenVerificationError::InvalidTokenLength);
            }

            return state.as_ref()
                .verify_token(token)
                .ok_or(TokenVerificationError::InvalidToken);
        }

        if let Some(query) = parts.uri.query() {
            if let Some(idx) = query.find(&C::HEADER_NAME.to_lowercase()) {
                return if let Some(token) = query.get((idx + 12)..(idx + 12 + C::TOKEN_LENGTH)) {
                    state.as_ref()
                        .verify_token(&HeaderValue::from_str(token).unwrap())
                        .ok_or(TokenVerificationError::InvalidToken)
                } else {
                    Err(TokenVerificationError::InvalidTokenLength)
                };
            }
        }

        Err(TokenVerificationError::MissingToken)
    }
}
