use std::{
    collections::HashMap, sync::Arc, time::Duration,
};

use axum::{
    async_trait,
    extract::{FromRef, FromRequestParts},
    http::{request::Parts, HeaderValue, StatusCode},
};
use parking_lot::Mutex;
use rand::{distributions::Alphanumeric, thread_rng, Rng};
use tokio::{
    spawn,
    sync::oneshot::{channel, Sender},
    time::sleep,
};

// #[derive(Clone, Hash)]
// pub struct Token<const N: u8>(HeaderValue);

// impl<const N: u8> Into<String> for Token<N> {
//     fn into(self) -> String {
//         unsafe { String::from_utf8_unchecked(self.0.as_bytes().into()) }
//     }
// }

// impl<const N: u8> TryFrom<HeaderValue> for Token<N> {
//     type Error = HeaderValue;

//     fn try_from(value: HeaderValue) -> Result<Self, Self::Error> {
//         if value.len() != N as usize {
//             Err(value)
//         } else {
//             Ok(Self(value))
//         }
//     }
// }


struct TokenGranterImpl<T: Send + Sync + 'static> {
    // When the sender gets dropped, the task responsible for expiring the token will complete
    tokens: Mutex<HashMap<HeaderValue, (Sender<()>, Arc<T>)>>,
    token_duration: Duration,
}


#[derive(Clone)]
pub struct BasicTokenGranter<T: Send + Sync + 'static> {
    inner: Arc<TokenGranterImpl<T>>
}

impl<T: Send + Sync + 'static> BasicTokenGranter<T> {
    pub fn new(token_duration: Duration) -> Self {
        Self {
            inner: Arc::new(TokenGranterImpl {
                tokens: Default::default(),
                token_duration,
            })
        }
    }

    pub fn create_token<const N: u8>(&self, item: T) -> HeaderValue {
        let bytes: Vec<u8> = thread_rng()
            .sample_iter(&Alphanumeric)
            .take(N as usize)
            .collect();

        let token = unsafe { HeaderValue::from_maybe_shared_unchecked(bytes) };

        let (sender, receiver) = channel();

        self.inner
            .tokens
            .lock()
            .insert(token.clone(), (sender, Arc::new(item)));

        let token2 = token.clone();
        let token_duration = self.inner.token_duration;
        let inner = self.inner.clone();

        spawn(async move {
            tokio::select! {
                () = sleep(token_duration) => {
                    inner.tokens.lock().remove(&token2);
                }
                _ = receiver => { }
            }
        });

        token
    }

    pub fn revoke_token(&self, token: &HeaderValue) -> Option<Arc<T>> {
        self.inner.tokens.lock().remove(token).unzip().1
    }

    pub fn verify_token(&self, token: &HeaderValue) -> Option<Arc<T>> {
        self
            .inner
            .tokens
            .lock()
            .get(token)
            .map(|x| x.1.clone())
    }
}

pub trait HeaderTokenGranter
{
    type AssociatedDataType: Send + Sync + 'static;
    const TOKEN_LENGTH: u8;
    const HEADER_NAME: &'static str;

    fn create_token(&self, item: Self::AssociatedDataType) -> HeaderValue;

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

            fn create_token(&self, item: Self::AssociatedDataType) -> HeaderValue {
                self.0.create_token::<{Self::TOKEN_LENGTH}>(item)
            }
        
            fn revoke_token(&self, token: &HeaderValue) -> Option<std::sync::Arc<Self::AssociatedDataType>> {
                self.0.revoke_token(token)
            }

            fn verify_token(&self, token: &HeaderValue) -> Option<std::sync::Arc<Self::AssociatedDataType>> {
                self.0.verify_token(token)
            }
        }

        impl $name {
            pub fn new(token_duration: std::time::Duration) -> Self {
                Self($crate::auth::token::BasicTokenGranter::new(token_duration))
            }
        }
    };
}

pub use create_header_token_granter;

pub struct VerifiedToken<TG: HeaderTokenGranter> {
    pub token: HeaderValue,
    pub item: Arc<TG::AssociatedDataType>,
    granter: TG
}


impl<TG: HeaderTokenGranter> VerifiedToken<TG> {
    pub fn revoke_token(self) {
        self.granter.revoke_token(&self.token);
    }
}


#[async_trait]
impl<S: Sync, TG: HeaderTokenGranter + FromRef<S>>
    FromRequestParts<S> for VerifiedToken<TG>
{
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
