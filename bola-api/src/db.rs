use aws_sdk_dynamodb::{model::AttributeValue, Client, error::{GetItemErrorKind, PutItemErrorKind}};
use aws_types::SdkConfig;
use mangle_api_core::{derive_more::Display, regex::Regex, serde::{Deserialize, self}};
use thiserror::Error;

#[derive(Debug, Default, Deserialize, Clone)]
#[serde(crate = "serde")]
pub struct UserProfile {
    pub username: String,
    pub easy_highscore: u16,
    pub normal_highscore: u16,
    pub expert_highscore: u16,
    pub tournament_wins: u16,
}

#[derive(Error, Debug, Display)]
pub enum GetItemError {
    AWSError(#[from] aws_sdk_dynamodb::error::GetItemError),
    ItemNotFound,
    EmptyItem,
    DeserializeError { field: &'static str },
}

#[derive(Error, Debug, Display)]
pub enum PutItemError {
    AWSError(#[from] aws_sdk_dynamodb::error::PutItemError),
}

#[derive(Error, Debug, Display)]
pub enum GetUserProfileError {
    GetItemError(#[from] GetItemError),
    NotAnEmail
}

#[derive(Error, Debug, Display)]
pub enum CreateUserProfileError {
    PutItemError(#[from] PutItemError),
    NotAnEmail
}


#[derive(Clone)]
pub struct DB {
    client: Client,
    bola_profiles_table: String,
    regex: Regex
}

impl DB {
    pub fn new(config: &SdkConfig, bola_profiles_table: String) -> Self {
        Self {
            client: Client::new(config),
            bola_profiles_table,
            regex: Regex::new(r"^.*@gmail.com$").expect("regex to be correct")
        }
    }

    pub async fn get_user_profile(
        &self,
        email: impl Into<String>,
    ) -> Result<UserProfile, GetUserProfileError> {
        let email = email.into();
        if !self.regex.is_match(&email) {
            return Err(GetUserProfileError::NotAnEmail)
        }

        let item = match self
            .client
            .get_item()
            .table_name(self.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email.into()))
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

        let map = item.item().ok_or(GetItemError::EmptyItem)?;

        macro_rules! deser {
            ($field: literal, $op: ident) => {
                map.get($field)
                    .map(|x| x.$op())
                    .transpose()
                    .map_err(|_| GetItemError::DeserializeError { field: $field })?
            };
            (num $field: literal) => {
                deser!($field, as_n)
                    .map(|x| x.parse())
                    .transpose()
                    .map_err(|_| GetItemError::DeserializeError { field: $field })?
                    .unwrap_or_default()
            };
        }

        Ok(UserProfile {
            easy_highscore: deser!(num "easy_highscore"),
            normal_highscore: deser!(num "normal_highscore"),
            expert_highscore: deser!(num "expert_highscore"),
            tournament_wins: deser!(num "tournament_wins"),
            username: deser!("username", as_s)
                .cloned()
                .ok_or(GetItemError::DeserializeError { field: "username" })?
        })
    }

    pub async fn create_user_profile(
        &self,
        email: impl Into<String>,
        profile: &UserProfile
    ) -> Result<(), CreateUserProfileError> {
        let email = email.into();
        if !self.regex.is_match(&email) {
            return Err(CreateUserProfileError::NotAnEmail)
        }

        match self
            .client
            .put_item()
            .table_name(self.bola_profiles_table.clone())
            .item("email", AttributeValue::S(email.into()))
            .item("easy_highscore", AttributeValue::N(profile.easy_highscore.to_string()))
            .item("normal_highscore", AttributeValue::N(profile.normal_highscore.to_string()))
            .item("expert_highscore", AttributeValue::N(profile.expert_highscore.to_string()))
            .item("tournament_wins", AttributeValue::N(profile.tournament_wins.to_string()))
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
