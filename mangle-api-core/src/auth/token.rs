use std::{hash::Hash, sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderValue, StatusCode},
    response::IntoResponse,
};
use bimap::BiMap;
use parking_lot::Mutex;
use rand::{distributions::Alphanumeric, thread_rng, Rng};

struct TokenEntry<T> {
    _sender: Sender<()>,
    item: Arc<T>,
}

impl<T: Hash> Hash for TokenEntry<T> {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.item.hash(state);
    }
}

impl<T: PartialEq> PartialEq for TokenEntry<T> {
    fn eq(&self, other: &Self) -> bool {
        self.item == other.item
    }
}

impl<T: Eq> Eq for TokenEntry<T> {}

struct TokenGranterImpl<C: TokenGranterConfig> {
    // When the sender gets dropped, the task responsible for expiring the token will complete
    tokens: Mutex<BiMap<Arc<HeaderValue>, TokenEntry<C::TokenDistinguisher>>>,
    token_duration: Duration,
}

pub trait TokenGranterConfig: Send + Sync + 'static {
    type TokenDistinguisher: Send + Sync + Hash + Eq + 'static;
    const TOKEN_LENGTH: usize;
}

pub trait HeaderTokenGranterConfig: TokenGranterConfig {
    const HEADER_NAME: &'static str;
}

pub struct TokenGranter<C: TokenGranterConfig> {
    inner: Arc<TokenGranterImpl<C>>,
}

impl<C: TokenGranterConfig> Clone for TokenGranter<C> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<C: TokenGranterConfig> TokenGranter<C> {
    pub fn new(token_duration: Duration) -> Self {
        Self {
            inner: Arc::new(TokenGranterImpl {
                tokens: Default::default(),
                token_duration,
            }),
        }
    }

    pub fn create_token(&self, item: C::TokenDistinguisher) -> VerifiedToken<C> {
        let mut lock = self.inner.tokens.lock();

        let (sender, receiver) = channel();
        let item = Arc::new(item);
        let entry = TokenEntry {
            _sender: sender,
            item: item.clone(),
        };
        lock.remove_by_right(&entry);

        let bytes: Vec<u8> = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(C::TOKEN_LENGTH)
            .collect();

        let token = Arc::new(unsafe { HeaderValue::from_maybe_shared_unchecked(bytes) });

        lock.insert(token.clone(), entry);

        let token_duration = self.inner.token_duration;
        let token2 = token.clone();
        let inner = self.inner.clone();

        spawn(async move {
            select! {
                () = sleep(token_duration) => {
                    inner.tokens.lock().remove_by_left(&token2);
                }
                _ = receiver => { }
            };
        });

        VerifiedToken { token, item }
    }

    pub fn revoke_token(&self, token: &HeaderValue) {
        self.inner.tokens.lock().remove_by_left(token);
    }

    pub fn verify_token(&self, token: &HeaderValue) -> Option<VerifiedToken<C>> {
        let mut lock = self.inner.tokens.lock();
        let (token, entry) = lock.remove_by_left(token)?;
        let item = entry.item.clone();
        lock.insert(token.clone(), entry);
        Some(VerifiedToken { token, item })
    }
}

// pub trait HeaderTokenGranter: Sized {
//     type AssociatedDataType: Send + Eq + Hash + Sync + 'static;
//     const TOKEN_LENGTH: u8;
//     const HEADER_NAME: &'static str;

//     fn create_token(
//         &self,
//         item: Self::AssociatedDataType,
//     ) -> VerifiedToken<Self>;

//     fn revoke_token(&self, token: &HeaderValue) -> Option<Arc<Self::AssociatedDataType>>;

//     fn verify_token(
//         &self,
//         token: &HeaderValue,
//     ) -> Option<(Arc<HeaderValue>, Arc<Self::AssociatedDataType>)>;
// }

// #[macro_export]
// macro_rules! create_header_token_granter {
//     ($vis: vis $name: ident $header_name: literal $token_length: literal $item_type: ty) => {
//         #[derive(Clone)]
//         $vis struct $name($crate::auth::token::BasicTokenGranter<$item_type>);

//         impl $crate::auth::token::HeaderTokenGranter for $name {
//             type AssociatedDataType = $item_type;
//             const TOKEN_LENGTH: u8 = $token_length;
//             const HEADER_NAME: &'static str = $header_name;

//             fn create_token(&self, item: Self::AssociatedDataType) -> $crate::auth::token::VerifiedToken<Self> {
//                 self.0.create_token::<{Self::TOKEN_LENGTH}>(item)
//             }

//             fn revoke_token(&self, token: &HeaderValue) -> Option<std::sync::Arc<Self::AssociatedDataType>> {
//                 self.0.revoke_token(token)
//             }

//             fn verify_token(&self, token: &HeaderValue) -> Option<(std::sync::Arc<axum::http::HeaderValue>, std::sync::Arc<Self::AssociatedDataType>)> {
//                 self.0.verify_token(token)
//             }
//         }

//         impl $name {
//             pub fn new(token_duration: Duration) -> Self {
//                 Self($crate::auth::token::BasicTokenGranter::new(token_duration))
//             }
//         }
//     };
// }

// pub use create_header_token_granter;
use tokio::{
    select, spawn,
    sync::oneshot::{channel, Sender},
    time::sleep,
};

pub struct VerifiedToken<C: TokenGranterConfig> {
    pub token: Arc<HeaderValue>,
    pub item: Arc<C::TokenDistinguisher>,
}

impl<C: TokenGranterConfig> VerifiedToken<C> {
    pub fn revoke_token(self, granter: TokenGranter<C>) {
        granter.revoke_token(&self.token);
    }
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
impl<S: Sync, C: HeaderTokenGranterConfig> FromRequestParts<S> for VerifiedToken<C>
where
    TokenGranter<C>: FromRef<S>,
{
    type Rejection = TokenVerificationError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(token) = parts.headers.get(C::HEADER_NAME) {
            if token.len() != C::TOKEN_LENGTH {
                return Err(TokenVerificationError::InvalidTokenLength);
            }

            return TokenGranter::from_ref(state)
                .verify_token(token)
                .ok_or(TokenVerificationError::InvalidToken);
        }

        if let Some(query) = parts.uri.query() {
            if let Some(idx) = query.find(&C::HEADER_NAME.to_lowercase()) {
                return if let Some(token) = query.get((idx + 12)..(idx + 12 + C::TOKEN_LENGTH)) {
                    TokenGranter::from_ref(state)
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
