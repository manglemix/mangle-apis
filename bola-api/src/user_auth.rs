use std::{sync::Arc, ops::Deref};

use aws_types::SdkConfig;
use aws_sdk_cognitoidentityprovider::{Client, model::AttributeType};
// use itertools::Itertools;
use mangle_api_core::{axum::{extract::{State, WebSocketUpgrade, ws::Message}, response::Response}, serde::{Deserialize, self}};
use mangle_api_core::ws::WsExt;
use mangle_api_core::serde_json::from_str;


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
struct SignUpData {
    username: String,
    password: String,
    email: String
}


pub async fn sign_up(State(user_auth): State<UserAuth>, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(|ws| async move {
        let Some((ws, msg)) = ws.easy_recv().await else {
            return
        };
        let username;

        if let Message::Text(msg) = msg {
            let Ok(SignUpData {
                username: tmp,
                password,
                email
            }) = from_str(&msg) else {
                ws.safe_drop().await;
                return
            };

            username = tmp;
            
            match user_auth.client
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
                .send()
                .await
            {
                Ok(resp) => if !resp.user_confirmed() {
                    ws.final_send(Message::Text("Failed".into())).await;
                    return
                }
                Err(_) => {
                    ws.safe_drop().await;
                    return
                }
            }
        } else {
            ws.safe_drop().await;
            return
        }

        // if user is confirmed, we wait for confirmation code
        let Some((ws, msg)) = ws.easy_recv().await else {
            return
        };

        if let Message::Text(msg) = msg {
            match user_auth.client
                .confirm_sign_up()
                .client_id(user_auth.client_id.deref())
                .username(&username)
                .confirmation_code(msg)
                .send()
                .await
            {
                Ok(_) => {
                    ws.final_send(Message::Text("Success".into())).await;
                }
                Err(_) => {
                    ws.safe_drop().await;
                }
            }
        } else {
            ws.safe_drop().await;
        }
    })
}