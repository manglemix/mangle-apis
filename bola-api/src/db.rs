use aws_types::SdkConfig;
use aws_sdk_dynamodb::{Client, model::AttributeValue};
use thiserror::{Error};
use mangle_api_core::derive_more::Display;


const BOLA_PROFILES_TABLE: &str = "bola_profiles";


#[derive(Debug)]
pub struct UserProfile {
    easy_highscore: u16,
    normal_highscore: u16,
    expert_highscore: u16,
    tournament_wins: u16
}


#[derive(Error, Debug, Display)]
pub enum GetItemError {
    AWSError(#[from] aws_sdk_dynamodb::error::GetItemError),
    EmptyItem,
    DeserializeError{ field: &'static str }
}


#[derive(Clone)]
pub struct DB {
    client: Client
}


impl DB {
    pub fn new(config: &SdkConfig) -> Self {
        Self {
            client: Client::new(config)
        }
    }

    pub async fn get_user_profile(&self, email: impl Into<String>) -> Result<UserProfile, GetItemError> {
        let item = self.client
            .get_item()
            .set_table_name(Some(BOLA_PROFILES_TABLE.to_string()))
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
                    .map_err(|_| GetItemError::DeserializeError{ field: $field })?
            };
            (num $field: literal) => {
                deser!($field, as_n)
                    .map(|x| x.parse())
                    .transpose()
                    .map_err(|_| GetItemError::DeserializeError{ field: $field })?
                    .unwrap_or_default()
            };
        }

        Ok(UserProfile {
            easy_highscore: deser!(num "easy_highscore"),
            normal_highscore: deser!(num "normal_highscore"),
            expert_highscore: deser!(num "expert_highscore"),
            tournament_wins: deser!(num "tournament_wins")
        })
    }
}
