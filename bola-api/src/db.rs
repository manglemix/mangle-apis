use std::collections::HashMap;

use anyhow::{anyhow, Error};
use aws_sdk_dynamodb::{
    error::GetItemErrorKind,
    model::{AttributeAction, AttributeValue, AttributeValueUpdate},
    Client,
};
use aws_types::SdkConfig;
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
        email: String,
        username: String,
        tournament_wins: Vec<u16>,
    ) -> Result<(), Error> {
        let mut req = self
            .client
            .put_item()
            .table_name(self.bola_profiles_table.clone())
            .item("email", AttributeValue::S(email))
            .item("easy_highscore", AttributeValue::N("0".into()))
            .item("normal_highscore", AttributeValue::N("0".into()))
            .item("expert_highscore", AttributeValue::N("0".into()))
            .item("username", AttributeValue::S(username))
            .item("unused", AttributeValue::N("0".into()));

        if !tournament_wins.is_empty() {
            req = req.item(
                "tournament_wins",
                AttributeValue::Ns(tournament_wins.iter().map(ToString::to_string).collect()),
            );
        }

        match req.send().await {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }

    pub async fn win_tournament(&self, week: u64, email: String) -> Result<(), Error> {
        let mut tournament_wins = self
            .client
            .get_item()
            .table_name(self.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email.clone()))
            .projection_expression("tournament_wins")
            .send()
            .await
            .map_err(|e| e.into_service_error())?
            .item()
            .and_then(|x| x.get("tournament_wins"))
            .map(|x| {
                x.as_ns().cloned().map_err(|_| {
                    Error::msg("Could not deserialize field: tournament_wins in user profile")
                })
            })
            .transpose()?
            .unwrap_or_default();

        let week = week.to_string();
        if !tournament_wins.contains(&week) {
            tournament_wins.push(week);
        }

        match self
            .client
            .update_item()
            .table_name(self.bola_profiles_table.clone())
            .key("email", AttributeValue::S(email))
            .attribute_updates(
                "tournament_wins",
                AttributeValueUpdate::builder()
                    .action(AttributeAction::Put)
                    .value(AttributeValue::Ns(tournament_wins))
                    .build(),
            )
            .send()
            .await
        {
            Ok(_) => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}
