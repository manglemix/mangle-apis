pub mod bearer;
#[cfg(feature = "oauth2")]
pub mod oauth2;
#[cfg(feature = "openid")]
pub mod openid;
pub mod token;

#[cfg(any(feature = "oauth2", feature = "openid"))]
pub mod auth_pages;
