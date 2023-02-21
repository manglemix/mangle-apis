use std::collections::HashMap;

use aws_sdk_dynamodb::{error::GetItemErrorKind, model::AttributeValue, Client};
use aws_types::SdkConfig;
use anyhow::{anyhow, Error};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize, Serialize, Clone)]
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

#[derive(Clone)]
pub struct DB {
    pub client: Client,
    pub bola_profiles_table: String,
}

impl DB {
    pub fn new(config: &SdkConfig, bola_profiles_table: String) -> Self {
        Self {
            client: Client::new(config),
            bola_profiles_table,
        }
    }

    pub async fn get_email_from_username(
        &self,
        username: impl Into<String>,
    ) -> Result<Option<String>, Error> {
        let username = username.into();

        let query = self
            .client
            .query()
            .table_name(self.bola_profiles_table.clone())
            .index_name("username-index")
            .key_condition_expression("username = :check_username")
            .expression_attribute_values(":check_username", AttributeValue::S(username.clone()))
            .projection_expression("email")
            .send()
            .await
            .map_err(Error::from)?;

        let Some(item) = query.items()
            .ok_or_else(|| anyhow!("query had no items field"))?
            .first() else
            {
                return Ok(None)
            };

        Ok(Some(
            item.get("email")
                .ok_or_else(|| anyhow!("user: {username} had no email!"))?
                .as_s()
                .map_err(|_| anyhow!("user: {username} had non-string email"))?
                .clone(),
        ))
    }

    pub async fn is_username_taken(&self, username: impl Into<String>) -> Result<bool, Error> {
        self.client
            .query()
            .table_name(self.bola_profiles_table.clone())
            .index_name("username-index")
            .key_condition_expression("username = :check_username")
            .expression_attribute_values(":check_username", AttributeValue::S(username.into()))
            .send()
            .await
            .map(|x| x.count() > 0)
            .map_err(Into::into)
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
    ) -> Result<Option<UserProfile>, Error> {
        let email = email.into();

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
            Err(e) => match &e.kind {
                GetItemErrorKind::ResourceNotFoundException(_) => return Ok(None),
                _ => Err(e),
            },
        }?;

        let Some(item) = item.item() else {
            return Ok(None)
        };

        Some(Self::map_to_user_profile(item)).transpose()
    }

    fn map_to_user_profile(map: &HashMap<String, AttributeValue>) -> Result<UserProfile, Error> {
        macro_rules! err {
            ($field_name: literal) => {
                anyhow!(
                    "Could not deserialize field: {} in user profile",
                    $field_name
                )
            };
        }
        macro_rules! deser {
            ($field: literal, $op: ident) => {
                map.get($field)
                    .map(|x| x.$op())
                    .transpose()
                    .map_err(|_| err!($field))?
            };
            (num $field: literal) => {
                deser!($field, as_n)
                    .map(|x| x.parse().map_err(|_| err!($field)))
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
                            out.push(num.parse().map_err(|_| err!("tournament_wins"))?);
                        }
                        out
                    }
                    None => vec![],
                }
            },
            username: deser!("username", as_s)
                .ok_or_else(|| anyhow!("Missing username in user profile"))?
                .clone(),
        })
    }

    pub async fn create_user_profile(
        &self,
        email: impl Into<String>,
        profile: UserProfile,
    ) -> Result<(), Error> {
        let email = email.into();

        let mut request = self
            .client
            .put_item()
            .table_name(self.bola_profiles_table.clone())
            .item("email", AttributeValue::S(email))
            .item(
                "easy_highscore",
                AttributeValue::N(profile.easy_highscore.to_string()),
            )
            .item(
                "normal_highscore",
                AttributeValue::N(profile.normal_highscore.to_string()),
            )
            .item(
                "expert_highscore",
                AttributeValue::N(profile.expert_highscore.to_string()),
            )
            .item("username", AttributeValue::S(profile.username));

        if !profile.tournament_wins.is_empty() {
            request = request.item(
                "tournament_wins",
                AttributeValue::Ns(
                    profile
                        .tournament_wins
                        .iter()
                        .map(ToString::to_string)
                        .collect(),
                ),
            );
        }

        match request.send().await {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
