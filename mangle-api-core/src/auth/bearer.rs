use axum::{http::{HeaderValue, Request, Response, StatusCode}, body::{HttpBody}};
use regex::RegexSet;
use tower_http::auth::AuthorizeRequest;
use std::{marker::PhantomData};
use constant_time_eq::constant_time_eq;


pub struct BearerAuth<ResBody> {
    api_token: HeaderValue,
    public_paths: RegexSet,
    _phantom: PhantomData<ResBody>
}


// Derive clone did not work
// It did too much introspection into the generic type, which actually does not need
// to implement Clone
impl<ResBody> Clone for BearerAuth<ResBody> {
    fn clone(&self) -> Self {
        Self { api_token: self.api_token.clone(), public_paths: self.public_paths.clone(), _phantom: self._phantom.clone() }
    }
}


impl<ResBody> BearerAuth<ResBody> {
    pub fn new(api_token: HeaderValue, public_paths: RegexSet) -> Self {
        Self {
            api_token,
            public_paths,
            _phantom: Default::default()
        }
    }
}


impl<ReqBody, ResBody> AuthorizeRequest<ReqBody> for BearerAuth<ResBody>
where
    ReqBody: HttpBody,
    ResBody: HttpBody + Default
{
    type ResponseBody = ResBody;

    fn authorize(&mut self, request: &mut Request<ReqBody>) -> Result<(), Response<Self::ResponseBody>> {
        macro_rules! unauthorized {
            () => {
                return Err(
                    Response::builder()
                        .status(StatusCode::UNAUTHORIZED)
                        .body(Default::default())
                        .unwrap()
                )
            };
        }

        if !self.public_paths.is_match(request.uri().path()) && 
            !request
                .headers()
                .get("Authorization")
                .map(|x| {
                    let x = match x.to_str() {
                        Ok(x) => x,
                        Err(_) => return false
                    };

                    if !x.starts_with("Bearer ") {
                        return false
                    }

                    let token = x.split_at(7).1;

                    constant_time_eq(token.as_bytes(), self.api_token.as_bytes())
                })
                .unwrap_or(false) {
            
            unauthorized!()
        }

        Ok(())
    }
}
