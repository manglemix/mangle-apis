use std::collections::HashMap;

use aws_sdk_dynamodb::{model::AttributeValue, Client, error::{GetItemErrorKind, PutItemErrorKind}};
use aws_types::SdkConfig;
use mangle_api_core::{derive_more::Display, serde::{Deserialize, self, Serialize}};
use thiserror::Error;
// use mangle_api_core::regex::Regex;

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
#[serde(crate = "serde")]
pub struct UserProfile {
    pub username: String,
    #[serde(default = "Default::default")]
    pub easy_highscore: u16,
    #[serde(default = "Default::default")]
    pub normal_highscore: u16,
    #[serde(default = "Default::default")]
    pub expert_highscore: u16,
    #[serde(default = "Default::default")]
    pub tournament_wins: Vec<u16>,
}

#[derive(Error, Debug, Display)]
pub enum CheckUsernameError {
    AWSError(#[from] aws_sdk_dynamodb::error::QueryError),
}

#[derive(Error, Debug, Display)]
pub enum GetItemError {
    AWSError(#[from] aws_sdk_dynamodb::error::GetItemError),
    ItemNotFound,
    DeserializeError { field: &'static str },
}

#[derive(Error, Debug, Display)]
pub enum PutItemError {
    AWSError(#[from] aws_sdk_dynamodb::error::PutItemError),
}

#[derive(Error, Debug, Display)]
pub enum GetUserProfileError {
    GetItemError(#[from] GetItemError),
    // NotAnEmail
}

#[derive(Error, Debug, Display)]
pub enum CreateUserProfileError {
    PutItemError(#[from] PutItemError),
    // NotAnEmail
}


#[derive(Clone)]
pub struct DB {
    client: Client,
    bola_profiles_table: String,
    // regex: Regex
}

impl DB {
    pub fn new(config: &SdkConfig, bola_profiles_table: String) -> Self {
        Self {
            client: Client::new(config),
            bola_profiles_table,
            // regex: Regex::new(r"^.*@gmail.com$").expect("regex to be correct")
        }
    }

    pub async fn is_username_taken(&self, username: impl Into<String>) -> Result<bool, CheckUsernameError> {
        match self
            .client
            .query()
            .table_name(self.bola_profiles_table.clone())
            .index_name("username-index")
            .key_condition_expression("username = :check_username")
            .expression_attribute_values(":check_username", AttributeValue::S(username.into()))
            .send()
            .await
            .map_err(|e| e.into_service_error())
        {
            Ok(x) => Ok(x.count() > 0),
            Err(e) => Err(CheckUsernameError::AWSError(e).into())
        }
    }

    // pub async fn get_user_profile_by_username(
    //     &self,
    //     username: impl Into<String>,
    // ) -> Result<UserProfile, GetUserProfileError> {
    //     let email = username.into();
    //     // if !self.regex.is_match(&email) {
    //     //     return Err(GetUserProfileError::NotAnEmail)
    //     // }

    //     let item = match self
    //         .client
    //         .query()
    //         .table_name(self.bola_profiles_table.clone())
    //         .index_name("username-index")
    //         .key_condition_expression("username = :check_username")
    //         .expression_attribute_values(":check_username", AttributeValue::S(username.into()))
    //         .send()
    //         .await
    //         .map_err(|e| e.into_service_error())
    //     {
    //         Ok(x) => Ok(x),
    //         Err(e) => Err(match &e.kind {
    //             QueryErrorKind::InternalServerError(_) => todo!(),
    //             QueryErrorKind::InvalidEndpointException(_) => todo!(),
    //             QueryErrorKind::ProvisionedThroughputExceededException(_) => todo!(),
    //             QueryErrorKind::RequestLimitExceeded(_) => todo!(),
    //             QueryErrorKind::ResourceNotFoundException(_) => todo!(),
    //             QueryErrorKind::Unhandled(_) => todo!(),
    //             _ => todo!(),
    //         })
    //     }?;

    //     Self::map_to_user_profile(
    //         item
    //             .items()
    //             .ok_or(GetItemError::ItemNotFound)?
    //             .get(0)
    //             .ok_or(GetItemError::ItemNotFound)?
    //     )
    // }

    pub async fn get_user_profile_by_email(
        &self,
        email: impl Into<String>,
    ) -> Result<UserProfile, GetUserProfileError> {
        let email = email.into();
        // if !self.regex.is_match(&email) {
        //     return Err(GetUserProfileError::NotAnEmail)
        // }

        let item = match self
            .client
            .get_item()
            .table_name(self.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email))
            .send()
            .await
            .map_err(|e| e.into_service_error())
        {
            Ok(x) => Ok(x),
            Err(e) => Err(match &e.kind {
                GetItemErrorKind::ResourceNotFoundException(_) => GetItemError::ItemNotFound,
                _ => GetItemError::AWSError(e)
            })
        }?;

        Self::map_to_user_profile(
            item.item().ok_or(GetItemError::ItemNotFound)?
        )
    }

    fn map_to_user_profile(map: &HashMap<String, AttributeValue>) -> Result<UserProfile, GetUserProfileError> {
        macro_rules! deser {
            ($field: literal, $op: ident) => {
                map.get($field)
                    .map(|x| x.$op())
                    .transpose()
                    .map_err(|_| GetItemError::DeserializeError { field: $field })?
            };
            (num $field: literal) => {
                deser!($field, as_n)
                    .map(|x| x.parse().map_err(|_| GetItemError::DeserializeError { field: $field }))
                    .transpose()
            };
        }

        Ok(UserProfile {
            easy_highscore: deser!(num "easy_highscore")?.unwrap_or_default(),
            normal_highscore: deser!(num "normal_highscore")?.unwrap_or_default(),
            expert_highscore: deser!(num "expert_highscore")?.unwrap_or_default(),
            tournament_wins: {
                match deser!("tournament_wins", as_ns) {
                    Some(nums) => {
                        let mut out = Vec::with_capacity(nums.len());
                        for num in nums {
                            out.push(
                                num.parse()
                                    .map_err(|_| GetItemError::DeserializeError { field: "tournament_wins" })?
                            );
                        }
                        out
                    }
                    None => vec![]
                }
            },
            username: deser!("username", as_s)
                .ok_or(GetItemError::DeserializeError { field: "username" })?
                .clone()
        })
    }

    pub async fn create_user_profile(
        &self,
        email: impl Into<String>,
        profile: UserProfile
    ) -> Result<(), CreateUserProfileError> {
        let email = email.into();
        // if !self.regex.is_match(&email) {
        //     return Err(CreateUserProfileError::NotAnEmail)
        // }

        let mut request = self
            .client
            .put_item()
            .table_name(self.bola_profiles_table.clone())
            .item("email", AttributeValue::S(email))
            .item("easy_highscore", AttributeValue::N(profile.easy_highscore.to_string()))
            .item("normal_highscore", AttributeValue::N(profile.normal_highscore.to_string()))
            .item("expert_highscore", AttributeValue::N(profile.expert_highscore.to_string()))
            .item("username", AttributeValue::S(profile.username));

        if !profile.tournament_wins.is_empty() {
            request = request
                .item(
                    "tournament_wins",
                    AttributeValue::Ns(profile.tournament_wins.iter().map(ToString::to_string).collect())
                );
        }

        match request
            .send()
            .await
            .map_err(|e| e.into_service_error())
        {
            Ok(_) => {}
            Err(e) => Err(match &e.kind {
                PutItemErrorKind::ResourceNotFoundException(_) => todo!(),
                _ => PutItemError::AWSError(e)
            })?
        };

        Ok(())
    }
}
