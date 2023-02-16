use aws_sdk_dynamodb::{model::AttributeValue, Client};
use aws_types::SdkConfig;
use mangle_api_core::derive_more::Display;
use thiserror::Error;

#[derive(Debug)]
pub struct UserProfile {
    easy_highscore: u16,
    normal_highscore: u16,
    expert_highscore: u16,
    tournament_wins: u16,
}

#[derive(Error, Debug, Display)]
pub enum GetItemError {
    AWSError(#[from] aws_sdk_dynamodb::error::GetItemError),
    EmptyItem,
    DeserializeError { field: &'static str },
}

#[derive(Clone)]
pub struct DB {
    client: Client,
    bola_profiles_table: String,
}

impl DB {
    pub fn new(config: &SdkConfig, bola_profiles_table: String) -> Self {
        Self {
            client: Client::new(config),
            bola_profiles_table,
        }
    }

    pub async fn get_user_profile(
        &self,
        email: impl Into<String>,
    ) -> Result<UserProfile, GetItemError> {
        let item = self
            .client
            .get_item()
            .table_name(self.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email.into()))
            .send()
            .await
            .map_err(|e| e.into_service_error())?;

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
        })
    }
}
