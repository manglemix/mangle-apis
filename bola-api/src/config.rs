use mangle_api_core::serde::Deserialize;

#[derive(Deserialize)]
#[serde(crate = "mangle_api_core::serde")]
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
    pub suspicious_security_log: String,
    #[serde(default = "Default::default")]
    pub cors_allowed_methods: Vec<String>,
    #[serde(default = "Default::default")]
    pub cors_allowed_origins: Vec<String>,

    pub google_client_secret_path: String,
    #[serde(default = "bola_profiles_table")]
    pub bola_profiles_table: String,
    pub oidc_redirect_base: String,
    // pub github_client_secret_path: String,
    pub api_token: String,
    #[serde(default = "token_duration")]
    pub token_duration: u64,
}

// impl BaseConfig for Config {
//     fn get_stderr_log_path(&self) -> mangle_api_core::anyhow::Result<&std::path::Path> {
//         Ok(self.stderr_log.as_ref())
//     }

//     fn get_routing_log_path(&self) -> mangle_api_core::anyhow::Result<&std::path::Path> {
//         Ok(self.routing_log.as_ref())
//     }

//     fn get_suspicious_security_log_path(&self) -> mangle_api_core::anyhow::Result<&std::path::Path> {
//         Ok(self.suspicious_security_log.as_ref())
//     }

//     fn get_cors_allowed_methods(&self) -> mangle_api_core::anyhow::Result<AllowMethods> {
//         let mut methods = Vec::new();

//         self.cors_allowed_methods
//             .iter()
//             .try_for_each(|x| {
//                 x.parse().map(|x| methods.push(x))
//             })?;

//         Ok(methods.into())
//     }

//     fn get_cors_allowed_origins(&self) -> mangle_api_core::anyhow::Result<AllowOrigin> {
//         let mut methods = Vec::new();

//         self.cors_allowed_origins
//             .iter()
//             .try_for_each(|x| {
//                 x.parse().map(|x| methods.push(x))
//             })?;

//         Ok(methods.into())
//     }

//     fn get_api_token(&self) -> mangle_api_core::anyhow::Result<HeaderValue> {
//         self.api_token.parse().map_err(Into::into)
//     }

//     fn get_bind_address(&self) -> mangle_api_core::anyhow::Result<BindAddress> {
//         format!("{}:{}", &self.server_address, self.server_port)
//             .parse()
//             .map_err(Into::into)
//             .map(BindAddress::Network)
//     }
// }

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
