use aws_config::load_from_env;
use aws_sdk_dynamodb::{Client, model::AttributeValue};


const BOLA_PROFILES_TABLE: &str = "bola_profiles";


#[derive(Clone)]
pub struct DB {
    client: Client
}


impl DB {
    pub async fn new() -> Self {
        Self {
            client: Client::new(&load_from_env().await)
        }
    }

    // pub async fn get_user_profile(&self, email: impl Into<String>) {
    //     self.client
    //         .get_item()
    //         .set_table_name(Some(BOLA_PROFILES_TABLE.to_string()))
    //         .key("email", AttributeValue::S(email.into()))
    //         .send()
    //         .await?;
            
    // }
}
