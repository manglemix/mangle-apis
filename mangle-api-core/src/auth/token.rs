use std::{collections::{HashMap}, sync::Arc, time::Duration, ops::Deref, marker::PhantomData, hash::Hash};

use axum::{http::{HeaderValue, StatusCode, request::Parts}, extract::{FromRequestParts, FromRef}, async_trait};
use parking_lot::Mutex;
use rand::{distributions::Alphanumeric, Rng, thread_rng};
use tokio::{spawn, time::sleep, sync::oneshot::{Sender, channel}};


#[derive(Clone, Hash)]
pub struct Token<const N: u8>(HeaderValue);


impl<const N: u8> Into<String> for Token<N> {
    fn into(self) -> String {
        unsafe {
            String::from_utf8_unchecked(self.0.as_bytes().into())
        }
    }
}


impl<const N: u8> TryFrom<HeaderValue> for Token<N> {
    type Error = HeaderValue;

    fn try_from(value: HeaderValue) -> Result<Self, Self::Error> {
        if value.len() != N as usize {
            Err(value)
        } else {
            Ok(Self(value))
        }
    }
}


#[derive(Clone)]
pub struct TokenGranter<const N: u8, T: Send + Sync + 'static> {
    // When the sender gets dropped, the task responsible for expiring the token will also complete
    tokens: Arc<Mutex<HashMap<HeaderValue, (Sender<()>, Arc<T>)>>>,
    token_duration: Duration,
}


impl<const N: u8, T: Send + Sync + 'static> TokenGranter<N, T> {
    pub fn new(token_duration: Duration) -> Self {
        Self {
            tokens: Default::default(),
            token_duration
        }
    }

    pub fn create_token(&self, item: T) -> Token<N> {
        let bytes: Vec<u8> = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(N as usize)
            .collect();
        
        let token = unsafe {
            Token(HeaderValue::from_maybe_shared_unchecked(bytes))
        };

        let (sender, receiver) = channel();

        self.tokens.lock().insert(token.0.clone(), (sender, Arc::new(item)));

        let token2 = token.clone();
        let token_duration = self.token_duration;
        let tokens = self.tokens.clone();

        spawn(async move {
            tokio::select! {
                () = sleep(token_duration) => {
                    tokens.lock().remove(&token2.0);
                }
                _ = receiver => { }
            }
        });

        token
    }

    pub fn revoke_token(&self, token: &Token<N>) -> Option<Arc<T>> {
        self.tokens.lock().remove(&token.0).unzip().1
    }
}


pub trait HeaderTokenGranter<const N: u8, T: Send + Sync + 'static>: Deref<Target=TokenGranter<N, T>> {
    const HEADER_NAME: &'static str;
}


#[macro_export]
macro_rules! create_header_token_granter {
    ($vis: vis $name: ident $header_name: literal $token_length: literal $item_type: ty) => {
        #[derive(Clone)]
        $vis struct $name($crate::auth::token::TokenGranter<$token_length, $item_type>);
        
        impl std::ops::Deref for $name {
            type Target = $crate::auth::token::TokenGranter<$token_length, $item_type>;

            fn deref(&self) -> &Self::Target {
                &self.0
            }
        }

        impl $crate::auth::token::HeaderTokenGranter<$token_length, $item_type> for $name {
            const HEADER_NAME: &'static str = $header_name;
        }

        impl $name {
            pub fn new(token_duration: std::time::Duration) -> Self {
                Self($crate::auth::token::TokenGranter::new(token_duration))
            }
        }
    };
}

pub use create_header_token_granter;


pub struct VerifiedToken<const N: u8, T: Send + Sync + 'static, TG: HeaderTokenGranter<N, T>> {
    pub token: Token<N>,
    pub item: Arc<T>,
    _phantom: PhantomData<TG>
}


#[async_trait]
impl<S: Sync, const N: u8, T: Send + Sync + 'static, TG: HeaderTokenGranter<N, T> + FromRef<S>> FromRequestParts<S> for VerifiedToken<N, T, TG> {
    type Rejection = (StatusCode, String);

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        if let Some(token) = parts.headers.get(TG::HEADER_NAME) {
            if token.len() != N as usize {
                return Err((StatusCode::BAD_REQUEST, "Invalid length for token".into()))
            }

            let tokens = TG::from_ref(state);
            let lock = tokens.tokens.lock();

            if let Some((_, item)) = lock.get(token) {
                Ok(Self {
                    token: Token(token.to_owned()),
                    item: item.clone(),
                    _phantom: Default::default()
                })
            } else {
                Err((StatusCode::UNAUTHORIZED, "Invalid or expired token".into()))
            }
        } else {
            Err((StatusCode::BAD_REQUEST, format!("Missing header: {}", TG::HEADER_NAME)))
        }
    }
}
