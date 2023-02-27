use std::{collections::HashMap, net::SocketAddr};

use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    #[serde(default = "default_server_address")]
    pub server_address: String,
    #[serde(default = "default_server_port")]
    pub server_port: u16,
    #[serde(default = "stderr_log")]
    pub stderr_log: String,
    #[serde(default = "routing_log")]
    pub routing_log: String,
    #[serde(default = "suspicious_security_log")]
    pub security_log: String,
    #[serde(default = "Default::default")]
    pub cors_allowed_methods: Vec<String>,
    #[serde(default = "Default::default")]
    pub cors_allowed_origins: Vec<String>,
    #[serde(default = "network_port")]
    pub network_port: u16,

    pub google_client_secret_path: String,
    #[serde(default = "bola_profiles_table")]
    pub bola_profiles_table: String,
    pub oidc_redirect_base: String,
    // pub github_client_secret_path: String,
    pub api_token: String,
    #[serde(default = "token_duration")]
    pub token_duration: u64,
    #[serde(default = "Default::default")]
    pub sibling_domains: HashMap<String, SocketAddr>,
}

fn default_server_address() -> String {
    "0.0.0.0".into()
}

fn default_server_port() -> u16 {
    80
}

fn stderr_log() -> String {
    "stderr.log".into()
}

fn routing_log() -> String {
    "routing.log".into()
}

fn suspicious_security_log() -> String {
    "suspicious_security.log".into()
}

fn bola_profiles_table() -> String {
    "bola_profiles".into()
}

fn token_duration() -> u64 {
    // 30 days
    60 * 60 * 24 * 30
}

fn network_port() -> u16 {
    10419
}
