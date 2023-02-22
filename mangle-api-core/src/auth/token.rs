use std::{hash::Hash, sync::Arc};

use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderValue, StatusCode},
};
use bimap::BiMap;
use parking_lot::Mutex;
use rand::{distributions::Alphanumeric, thread_rng, Rng};


pub struct TokenHandle<T: Send + Sync + Hash + Eq + 'static> {
    pub token: Arc<HeaderValue>,
    pub item: Arc<T>,
    granter: Arc<TokenGranterImpl<T>>
}

impl<T: Send + Sync + Hash + Eq + 'static> Drop for TokenHandle<T> {
    fn drop(&mut self) {
        self.granter.tokens.lock().remove_by_left(&self.token);
    }
}


struct TokenGranterImpl<T: Send + Sync + Hash + Eq + 'static> {
    // When the sender gets dropped, the task responsible for expiring the token will complete
    tokens: Mutex<BiMap<Arc<HeaderValue>, Arc<T>>>
}

impl<T: Send + Sync + Hash + Eq + 'static> Default for TokenGranterImpl<T> {
    fn default() -> Self {
        Self { tokens: Default::default() }
    }
}


#[derive(Clone)]
pub struct BasicTokenGranter<T: Send + Sync + Hash + Eq + 'static> {
    inner: Arc<TokenGranterImpl<T>>,
}

impl<T: Send + Sync + Hash + Eq + 'static> Default for BasicTokenGranter<T> {
    fn default() -> Self {
        Self { inner: Default::default() }
    }
}


impl<T: Send + Sync + Hash + Eq + 'static> BasicTokenGranter<T> {
    pub fn create_token<const N: u8>(&self, item: T) -> TokenHandle<T> {
        let mut lock = self.inner.tokens.lock();
        lock.remove_by_right(&item);

        let bytes: Vec<u8> = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(N as usize)
            .collect();

        let token = Arc::new(unsafe { HeaderValue::from_maybe_shared_unchecked(bytes) });

        let item = Arc::new(item);
        lock.insert(token.clone(), item.clone());

        TokenHandle { token , granter: self.inner.clone(), item }
    }

    pub fn revoke_token(&self, token: &HeaderValue) -> Option<Arc<T>> {
        self.inner
            .tokens
            .lock()
            .remove_by_left(token)
            .unzip()
            .1
    }

    pub fn verify_token(&self, token: &HeaderValue) -> Option<Arc<T>> {
        self.inner
            .tokens
            .lock()
            .get_by_left(token)
            .cloned()
    }
}

pub trait HeaderTokenGranter {
    type AssociatedDataType: Send + Eq + Hash + Sync + 'static;
    const TOKEN_LENGTH: u8;
    const HEADER_NAME: &'static str;

    fn create_token(&self, item: Self::AssociatedDataType) -> TokenHandle<Self::AssociatedDataType>;

    fn revoke_token(&self, token: &HeaderValue) -> Option<Arc<Self::AssociatedDataType>>;

    fn verify_token(&self, token: &HeaderValue) -> Option<Arc<Self::AssociatedDataType>>;
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

            fn create_token(&self, item: Self::AssociatedDataType) -> $crate::auth::token::TokenHandle<Self::AssociatedDataType> {
                self.0.create_token::<{Self::TOKEN_LENGTH}>(item)
            }

            fn revoke_token(&self, token: &HeaderValue) -> Option<std::sync::Arc<Self::AssociatedDataType>> {
                self.0.revoke_token(token)
            }

            fn verify_token(&self, token: &HeaderValue) -> Option<std::sync::Arc<Self::AssociatedDataType>> {
                self.0.verify_token(token)
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self(Default::default())
            }
        }
    };
}

pub use create_header_token_granter;

pub struct VerifiedToken<TG: HeaderTokenGranter> {
    pub token: HeaderValue,
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
            if let Some(item) = granter.verify_token(token) {
                Ok(Self {
                    token: token.to_owned(),
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
