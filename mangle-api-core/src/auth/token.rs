use std::{hash::Hash, sync::Arc, time::Duration};

use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderValue, StatusCode},
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

struct TokenGranterImpl<T: Send + Sync + Hash + Eq + 'static> {
    // When the sender gets dropped, the task responsible for expiring the token will complete
    tokens: Mutex<BiMap<Arc<HeaderValue>, TokenEntry<T>>>,
    token_duration: Duration,
}

#[derive(Clone)]
pub struct BasicTokenGranter<T: Send + Sync + Hash + Eq + 'static> {
    inner: Arc<TokenGranterImpl<T>>,
}

impl<T: Send + Sync + Hash + Eq + 'static> BasicTokenGranter<T> {
    pub fn new(token_duration: Duration) -> Self {
        Self {
            inner: Arc::new(TokenGranterImpl {
                tokens: Default::default(),
                token_duration,
            }),
        }
    }

    pub fn create_token<const N: u8>(&self, item: T) -> (Arc<HeaderValue>, Arc<T>) {
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
            .take(N as usize)
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

        (token, item)
    }

    pub fn revoke_token(&self, token: &HeaderValue) -> Option<Arc<T>> {
        self.inner
            .tokens
            .lock()
            .remove_by_left(token)
            .map(|(_, x)| x.item)
    }

    pub fn verify_token(&self, token: &HeaderValue) -> Option<(Arc<HeaderValue>, Arc<T>)> {
        let mut lock = self.inner.tokens.lock();
        let (token, entry) = lock.remove_by_left(token)?;
        let item = entry.item.clone();
        lock.insert(token.clone(), entry);
        Some((token, item))
    }
}

pub trait HeaderTokenGranter {
    type AssociatedDataType: Send + Eq + Hash + Sync + 'static;
    const TOKEN_LENGTH: u8;
    const HEADER_NAME: &'static str;

    fn create_token(
        &self,
        item: Self::AssociatedDataType,
    ) -> (Arc<HeaderValue>, Arc<Self::AssociatedDataType>);

    fn revoke_token(&self, token: &HeaderValue) -> Option<Arc<Self::AssociatedDataType>>;

    fn verify_token(
        &self,
        token: &HeaderValue,
    ) -> Option<(Arc<HeaderValue>, Arc<Self::AssociatedDataType>)>;
}

#[macro_export]
macro_rules! create_header_token_granter {
    ($vis: vis $name: ident $header_name: literal $token_length: literal $item_type: ty) => {
        #[derive(Clone)]
        $vis struct $name($crate::auth::token::BasicTokenGranter<$item_type>);

        impl $crate::auth::token::HeaderTokenGranter for $name {
            type AssociatedDataType = $item_type;
            const TOKEN_LENGTH: u8 = $token_length;
            const HEADER_NAME: &'static str = $header_name;

            fn create_token(&self, item: Self::AssociatedDataType) -> (std::sync::Arc<axum::http::HeaderValue>, std::sync::Arc<Self::AssociatedDataType>) {
                self.0.create_token::<{Self::TOKEN_LENGTH}>(item)
            }

            fn revoke_token(&self, token: &HeaderValue) -> Option<std::sync::Arc<Self::AssociatedDataType>> {
                self.0.revoke_token(token)
            }

            fn verify_token(&self, token: &HeaderValue) -> Option<(std::sync::Arc<axum::http::HeaderValue>, std::sync::Arc<Self::AssociatedDataType>)> {
                self.0.verify_token(token)
            }
        }

        impl $name {
            pub fn new(token_duration: Duration) -> Self {
                Self($crate::auth::token::BasicTokenGranter::new(token_duration))
            }
        }
    };
}

pub use create_header_token_granter;
use tokio::{
    select, spawn,
    sync::oneshot::{channel, Sender},
    time::sleep,
};

pub struct VerifiedToken<TG: HeaderTokenGranter> {
    pub token: Arc<HeaderValue>,
    pub item: Arc<TG::AssociatedDataType>,
    granter: TG,
}

impl<TG: HeaderTokenGranter> VerifiedToken<TG> {
    pub fn revoke_token(self) {
        self.granter.revoke_token(&self.token);
    }
}

#[async_trait]
impl<S: Sync, TG: HeaderTokenGranter + FromRef<S>> FromRequestParts<S> for VerifiedToken<TG> {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(token) = parts.headers.get(TG::HEADER_NAME) {
            if token.len() != TG::TOKEN_LENGTH as usize {
                return Err((StatusCode::BAD_REQUEST, "Invalid length for token".into()));
            }

            let granter = TG::from_ref(state);
            if let Some((token, item)) = granter.verify_token(token) {
                Ok(Self {
                    token,
                    item,
                    granter,
                })
            } else {
                Err((StatusCode::UNAUTHORIZED, "Invalid or expired token".into()))
            }
        } else {
            Err((
                StatusCode::BAD_REQUEST,
                format!("Missing header: {}", TG::HEADER_NAME),
            ))
        }
    }
}
