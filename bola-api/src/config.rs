use std::{collections::HashMap, net::SocketAddr, time::Duration};

use mangle_api_core::BindAddress;
use serde::Deserialize;

#[derive(Deserialize)]
pub struct Config {
    pub bind_address: BindAddress,
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
    pub token_duration: Duration,
    #[serde(default = "Default::default")]
    pub sibling_domains: HashMap<String, SocketAddr>,

    pub start_week_time: Duration,

    #[serde(default = "stylesheet_path")]
    pub stylesheet_path: String,
    #[serde(default = "invalid_path")]
    pub invalid_path: String,
    #[serde(default = "invalid_path")]
    pub internal_error_path: String,
    #[serde(default = "success_path")]
    pub success_path: String,
    #[serde(default = "late_path")]
    pub late_path: String,

    #[serde(default = "Default::default")]
    pub https: bool,
    #[serde(default = "Default::default")]
    pub https_domain: String,
    #[serde(default = "certs_path")]
    pub certs_path: String,
    #[serde(default = "key_path")]
    pub key_path: String,
}

fn stderr_log() -> String {
    "stderr.log".into()
}

fn routing_log() -> String {
    "routing.log".into()
}

fn suspicious_security_log() -> String {
    "security.log".into()
}

fn bola_profiles_table() -> String {
    "bola_profiles".into()
}

fn token_duration() -> Duration {
    // 30 days
    Duration::from_secs(60 * 60 * 24 * 30)
}

fn network_port() -> u16 {
    10419
}

fn stylesheet_path() -> String {
    "manglemix.css".into()
}

fn invalid_path() -> String {
    "invalid.html".into()
}

fn success_path() -> String {
    "success.html".into()
}

fn late_path() -> String {
    "late.html".into()
}

fn certs_path() -> String {
    "https/certs.pem".into()
}

fn key_path() -> String {
    "https/key.pem".into()
}
