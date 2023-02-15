use std::{sync::Arc, ops::Deref};

use aws_types::SdkConfig;
use aws_sdk_cognitoidentityprovider::{Client, model::{AttributeType, UserStatusType}};
// use itertools::Itertools;
use mangle_api_core::{axum::{extract::{State}, http::StatusCode, Form}, serde::{Deserialize, self}, log::error};


// #[derive(Error, Debug, Display)]
// pub enum LoginError {
//     AWSError(#[from] SdkError<InitiateAuthError>)
// }


// struct UserAuthImpl {
//     client_id: String
// }


#[derive(Clone)]
pub struct UserAuth {
    client: Client,
    client_id: Arc<String>
    // auth_impl: Arc<UserAuthImpl>
}


impl UserAuth {
    pub fn new(config: &SdkConfig, client_id: String) -> Self {
        Self {
            client: Client::new(config),
            client_id: Arc::new(client_id)
        }
    }

    // pub async fn sign_up_user(&self, username: impl Into<String>, password: impl Into<String>) -> Result<SignUpFinalizer, SdkError<SignUpError>> {
    //     let username = username.into();

    //     match self.client
    //         .sign_up()
    //         .client_id(&self.auth_impl.client_id)
    //         .username(username.clone())
    //         .password(password)
    //         .send()
    //         .await
    //     {
    //         Ok(x) => {
    //             println!("{x:?}");
    //             Ok(SignUpFinalizer {
    //                 user_auth: self,
    //                 username
    //             })
    //         }
    //         Err(e) => Err(e)
    //     }
    // }

    // pub async fn login_user(&self, username: impl Into<String>, password: impl Into<String>) -> Result<(), LoginError> {
    //     println!("{:?}", self.client
    //         .initiate_auth()
    //         .client_id(&self.auth_impl.client_id)
    //         .auth_flow(AuthFlowType::UserPasswordAuth)
    //         .auth_parameters("USERNAME", username)
    //         .auth_parameters("PASSWORD", password)
    //         .send()
    //         .await)
    // }
}


// pub struct SignUpFinalizer<'a> {
//     user_auth: &'a UserAuth,
//     username: String
// }


// impl<'a> SignUpFinalizer<'a> {
//     pub async fn finalize_sign_up(self, confirmation_code: impl Into<String>) -> Result<(), SdkError<ConfirmSignUpError>> {
//         self.user_auth
//             .client
//             .confirm_sign_up()
//             .client_id(&self.user_auth.auth_impl.client_id)
//             .username(self.username)
//             .confirmation_code(confirmation_code)
//             .send()
//             .await
//             .map(|x| {
//                 println!("{x:?}");
//                 ()
//             })
//     }
// }


#[derive(Deserialize)]
#[serde(crate="serde")]
pub struct SignUpData {
    username: String,
    password: String,
    email: String,
    nickname: Option<String>
}


pub async fn sign_up(
    State(user_auth): State<UserAuth>,
    Form(SignUpData {
        username,
        password,
        email,
        nickname
    }): Form<SignUpData>
) -> (StatusCode, String) {
    if let Err(e) = user_auth.client
        .sign_up()
        .client_id(user_auth.client_id.deref())
        .username(&username)
        .password(password)
        .user_attributes(
            AttributeType::builder()
                .name("email")
                .value(email)
                .build()
        )
        .user_attributes(
            AttributeType::builder()
                .name("nickname")
                .value(nickname.as_ref().unwrap_or(&username))
                .build()
        )
        .send()
        .await
    {
        use aws_sdk_cognitoidentityprovider::error::SignUpErrorKind::*;
        return match e.into_service_error().kind {
            InvalidPasswordException(e) => (StatusCode::BAD_REQUEST, e.to_string()),
            CodeDeliveryFailureException(_) => (StatusCode::INTERNAL_SERVER_ERROR, "Message failed to deliver".into()),
            UsernameExistsException(_) => match user_auth.client
                    .admin_get_user()
                    .username(&username)
                    .user_pool_id("BolaUsers")
                    .send()
                    .await
                {
                    Ok(x) => if let Some(&UserStatusType::Unconfirmed) = x.user_status()
                        {
                            if let Err(e) = user_auth.client
                                .admin_delete_user()
                                .username(&username)
                                .send()
                                .await
                            {
                                error!(target: "user_auth", "{e:?}");
                                (StatusCode::INTERNAL_SERVER_ERROR, String::new())
                            } else {
                                (StatusCode::OK, String::new())
                            }
                        } else {
                            (StatusCode::BAD_REQUEST, "Username exists".into())
                        }
                    Err(e) => {
                        error!(target: "user_auth", "{e:?}");
                        (StatusCode::INTERNAL_SERVER_ERROR, String::new())
                    }
                }
            e => {
                error!(target: "user_auth", "{e:?}");
                (StatusCode::INTERNAL_SERVER_ERROR, String::new())
            }
        }
    }
    (StatusCode::OK, String::new())
}