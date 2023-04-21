#![feature(string_leak)]
#![feature(never_type)]
#![feature(associated_type_bounds)]
#![feature(exclusive_wrapper)]
#![feature(arbitrary_self_types)]

use axum::{http::HeaderValue, routing::MethodRouter, Router, Server};

pub mod auth;
pub mod distributed;
pub mod neo_api;
pub mod tls;
pub mod webrtc;
pub mod ws;

#[cfg(any(feature = "redis"))]
pub mod db;

use anyhow::{Context, Error, Result};
use clap::{arg, builder::IntoResettable, ArgMatches, Command};
use lers::{solver::Http01Solver, Directory, LETS_ENCRYPT_PRODUCTION_URL};
use messagist::{
    pipes::{
        start_connection, start_listener, ListenerErrorHandler,
        ToLocalSocketName,
    },
    ExclusiveMessageHandler,
};
use std::{future::{pending, Future}, fmt::Display};

use fern::{log_file, Dispatch};
use log::{error, info, warn, LevelFilter};
use parking_lot::Mutex;
use regex::{Regex, RegexSet};
use serde::{de::DeserializeOwned, Deserialize};
use std::{
    env,
    ffi::OsString,
    fs::{read_to_string, File},
    future::Pending,
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    sync::Arc,
};
pub use tokio_native_tls::native_tls::Identity;
use toml::from_str;
use tower::ServiceBuilder;
use tower_http::{
    auth::RequireAuthorizationLayer,
    compression::CompressionLayer,
    cors::{AllowMethods, AllowOrigin, CorsLayer},
    trace::TraceLayer,
};

use auth::bearer::BearerAuth;

pub use bimap;
pub use fern;
pub use parking_lot;
pub use rand;
#[cfg(any(feature = "redis"))]
pub use redis;
pub use regex;
pub use serde_json;
pub use toml;
pub use tower_http;

use crate::tls::TlsAcceptor;

mod log_targets {
    pub const SECURITY: &str = "suspicious_security";
}
const ROUTING_REGEX_RAW: &str = "^(tower_http::trace|hyper::proto|mio|tracing|routing)";

// Setup logger
static CRITICAL_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Info);
static STDERR_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Info);
static ROUTING_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Info);

pub fn make_app<const N: usize>(
    name: &'static str,
    version: impl IntoResettable<clap::builder::Str>,
    about: &'static str,
    extra_log_targets: [&'static str; N],
) -> Command {
    Command::new(name)
        .version(version)
        .author("manglemix")
        .about(about)
        .subcommand(
            Command::new("start")
                .about("Starts the web server in the current directory")
                .arg(arg!([config_path] "An optional path to a config file")),
        )
        .subcommand(
            Command::new("log_level")
                .about("Sets or gets the log level of a specific log target")
                .arg(
                    arg!(<target> "The logging target to set or get").value_parser(
                        ["stderr", "routing"]
                            .into_iter()
                            .chain(extra_log_targets)
                            .collect::<Vec<_>>(),
                    ),
                )
                .arg(
                    arg!([new_level] "If provided, will set the log level for the given target")
                        .value_parser(["off", "error", "warn", "info", "debug", "trace"]),
                ),
        )
        .subcommand(Command::new("status").about("Checks the status of the server"))
        .subcommand(Command::new("stop").about("Stops the currently running server"))
}

#[derive(Deserialize, Clone)]
pub enum BindAddress {
    #[serde(rename = "local")]
    Local(String),
    #[serde(rename = "http")]
    HTTP(IpAddr),
    #[serde(rename = "network")]
    Network(SocketAddr),
}

pub fn get_pipe_name(pipe_name_env_var: &'static str, default_pipe_name: &'static str) -> OsString {
    match env::var_os(pipe_name_env_var) {
        Some(x) => x,
        None => default_pipe_name.into(),
    }
}

pub enum CommandMatchResult<'a, Config> {
    StartProgram(Config),
    Unmatched((&'a str, &'a ArgMatches)),
}

pub async fn pre_matches<'a, Config>(
    matches: &ArgMatches,
    pipe_name: impl ToLocalSocketName<'a>,
    on_active_msg: Option<String>,
) -> Result<CommandMatchResult<Config>>
where
    Config: DeserializeOwned,
{
    match matches.subcommand() {
        Some(("start", matches)) => {
            if start_connection(pipe_name).await.is_ok() {
                return Err(if let Some(msg) = on_active_msg {
                    Error::msg(msg)
                } else {
                    Error::msg("A server may be active")
                });
            }
            let config_path: String = matches
                .get_one("config_path")
                .cloned()
                .unwrap_or("configs.toml".into());
            let err_msg = format!("Reading configuration file: {config_path}");
            from_str(&read_to_string(config_path).context(err_msg.clone())?)
                .context(err_msg)
                .map(CommandMatchResult::StartProgram)
        }
        Some((name, matches)) => Ok(CommandMatchResult::Unmatched((name, matches))),
        None => Err(Error::msg(
            "You need to type a command as an argument! Use -h for more information",
        )),
    }
}

pub fn setup_logger(
    stderr_log_path: &str,
    routing_log_path: &str,
    security_log_path: &str,
) -> Result<Dispatch> {
    let routing_regex = Regex::new(ROUTING_REGEX_RAW).unwrap();
    let non_stderr = Arc::new(
        RegexSet::new([
            ROUTING_REGEX_RAW.to_string(),
            format!("^{}", log_targets::SECURITY),
        ])
        .unwrap(),
    );
    let non_stderr2 = non_stderr.clone();

    Ok(Dispatch::new()
        .format(|out, message, record| {
            out.finish(format_args!(
                "{}[{}][{}:{}] {}",
                chrono::Local::now().format("[%Y-%m-%d][%H:%M:%S]"),
                record.level(),
                record.target(),
                record
                    .line()
                    .as_ref()
                    .map(ToString::to_string)
                    .unwrap_or("?".into()),
                message
            ))
        })
        // Critical-Only Stderr to Stderr
        .chain(
            Dispatch::new()
                .filter(move |metadata| {
                    !non_stderr.is_match(metadata.target())
                        && metadata.level() <= *CRITICAL_LOG_LEVEL.lock()
                })
                .chain(std::io::stderr()),
        )
        // All Stderr to file
        .chain(
            Dispatch::new()
                .filter(move |metadata| {
                    !non_stderr2.is_match(metadata.target())
                        && metadata.level() <= *STDERR_LOG_LEVEL.lock()
                })
                .chain(
                    log_file(stderr_log_path).context(format!("Opening {:?}", stderr_log_path))?,
                ),
        )
        // Routing to file
        .chain(
            Dispatch::new()
                .filter(move |metadata| {
                    routing_regex.is_match(metadata.target())
                        && metadata.level() <= *ROUTING_LOG_LEVEL.lock()
                })
                .chain(
                    log_file(routing_log_path)
                        .context(format!("Opening {:?}", routing_log_path))?,
                ),
        )
        // Suspicious security to file (maybe more?)
        .chain(
            Dispatch::new()
                .filter(|metadata| metadata.target().starts_with(log_targets::SECURITY))
                .chain(
                    log_file(security_log_path)
                        .context(format!("Opening {:?}", security_log_path))?,
                ),
        ))
}

pub async fn get_https_credentials(
    bind_address: BindAddress,
    certs_path: &str,
    key_path: &str,
    https_email: String,
    https_domain: String,
) -> Result<Identity> {
    let mut certs = vec![];
    let mut key = vec![];

    match File::open(certs_path) {
        Ok(mut file) => {
            file.read_to_end(&mut certs)
                .context(format!("Reading {}", certs_path))?;
        }
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => {}
            _ => return Err(e).context(format!("Opening {}", certs_path)),
        },
    }
    match File::open(key_path) {
        Ok(mut file) => {
            file.read_to_end(&mut key)
                .context(format!("Reading {}", key_path))?;
        }
        Err(e) => match e.kind() {
            std::io::ErrorKind::NotFound => {}
            _ => return Err(e).context(format!("Opening {}", key_path)),
        },
    }

    if certs.is_empty() {
        warn!("No certs were found, obtaining...");
        if let BindAddress::Network(mut address) = bind_address {
            let solver = Http01Solver::new();
            address.set_port(80);
            let handle = solver
                .start(&address)
                .context(format!("Binding ACME solver to {address}"))?;

            // Create a new directory for Let's Encrypt Production
            let directory = Directory::builder(LETS_ENCRYPT_PRODUCTION_URL)
                .http01_solver(Box::new(solver))
                .build()
                .await
                .context("Building ACME directory")?;

            // Create an ACME account to order your certificate. In production, you should store
            // the private key, so you can renew your certificate.
            let account = directory
                .account()
                .terms_of_service_agreed(true)
                .contacts(vec![format!("mailto:{https_email}")])
                .create_if_not_exists()
                .await
                .context("Creating ACME account")?;

            // Obtain your certificate
            let certificate = account
                .certificate()
                .add_domain(https_domain)
                .obtain()
                .await
                .context("Collecting certificate")?;

            certs = certificate
                .fullchain_to_pem()
                .context("Converting certificate to pem")?;

            key = certificate
                .private_key_to_pem()
                .context("Converting private key to pem")?;

            handle.stop().await.context("Stopping ACME handle")?;

            File::create(certs_path)
                .context(format!("Opening {}", certs_path))?
                .write_all(&certs)
                .context(format!("Writing to {}", certs_path))?;

            File::create(key_path)
                .context(format!("Opening {}", key_path))?
                .write_all(&key)
                .context(format!("Writing to {}", key_path))?;
        } else {
            return Err(Error::msg(
                "Failed to replace missing credentials as we are binded locally",
            ));
        }
    }

    Identity::from_pkcs8(&certs, &key).context("Loading HTTPS Credentials")
}

#[derive(Clone, Copy)]
pub struct Unset;

pub struct API<
    S = Unset,
    P = Unset,
    AT = Unset,
    BA = Unset,
    const N1: usize = 0,
    const N2: usize = 0,
    H = Unset,
    Fut = Pending<String>,
> {
    state: S,
    pipe_name: P,
    cors_allowed_methods: AllowMethods,
    cors_allowed_origins: AllowOrigin,
    api_token: AT,
    bind_address: BA,
    public_paths: [&'static str; N1],
    routes: [(&'static str, MethodRouter<S>); N2],
    https_identity: Option<Identity>,
    control_handler: H,
    concurrent_fut: Fut,
}

pub fn new_api(
) -> API<Unset, Unset, Unset, Unset, 0, 0, Unset, Pending<()>> {
    API {
        state: Unset,
        pipe_name: Unset,
        cors_allowed_methods: AllowMethods::from([]),
        cors_allowed_origins: AllowOrigin::from([]),
        api_token: Unset,
        bind_address: Unset,
        public_paths: [],
        routes: [],
        https_identity: None,
        control_handler: Unset,
        concurrent_fut: pending(),
    }
}

impl<S, P, AT, BO, const N1: usize, const N2: usize, H, Fut>
    API<S, P, AT, BO, N1, N2, H, Fut>
{
    /// Sets the state used by this API
    /// # Warning
    /// Setting the state removes all existing routes
    pub fn set_state<S2>(self, state: S2) -> API<S2, P, AT, BO, N1, 0, H, Fut>
    where
        S2: Clone + Send + Sync + 'static,
    {
        API {
            state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: [],
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_pipe_name(self, pipe_name: OsString) -> API<S, OsString, AT, BO, N1, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_cors_allowed_methods(
        self,
        cors_allowed_methods: impl Into<AllowMethods>,
    ) -> API<S, P, AT, BO, N1, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: cors_allowed_methods.into(),
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_cors_allowed_origins(
        self,
        cors_allowed_origins: impl Into<AllowOrigin>,
    ) -> API<S, P, AT, BO, N1, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: cors_allowed_origins.into(),
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_api_token(
        self,
        api_token: HeaderValue,
    ) -> API<S, P, HeaderValue, BO, N1, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_bind_address(
        self,
        bind_address: BindAddress,
    ) -> API<S, P, AT, BindAddress, N1, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_public_paths<const N1_2: usize>(
        self,
        public_paths: [&'static str; N1_2],
    ) -> API<S, P, AT, BO, N1_2, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_routes<const N2_2: usize>(
        self,
        routes: [(&'static str, MethodRouter<S>); N2_2],
    ) -> API<S, P, AT, BO, N1, N2_2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_https_identity(
        self,
        https_identity: Identity,
    ) -> API<S, P, AT, BO, N1, N2, H, Fut> {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: Some(https_identity),
            control_handler: self.control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_control_handler<H2>(
        self,
        control_handler: H2,
    ) -> API<S, P, AT, BO, N1, N2, H2, Fut>
    where
        H2: ExclusiveMessageHandler<SessionState = ()>
            + Send
            + ListenerErrorHandler
            + 'static,
    {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler,
            concurrent_fut: self.concurrent_fut,
        }
    }
    pub fn set_concurrent_future<Fut2>(
        self,
        concurrent_fut: Fut2,
    ) -> API<S, P, AT, BO, N1, N2, H, Fut2>
    where
        Fut2: Future<Output: Display>,
    {
        API {
            state: self.state,
            pipe_name: self.pipe_name,
            cors_allowed_methods: self.cors_allowed_methods,
            cors_allowed_origins: self.cors_allowed_origins,
            api_token: self.api_token,
            bind_address: self.bind_address,
            public_paths: self.public_paths,
            routes: self.routes,
            https_identity: self.https_identity,
            control_handler: self.control_handler,
            concurrent_fut,
        }
    }
}

impl<S, const N1: usize, const N2: usize, H, Fut>
    API<S, OsString, HeaderValue, BindAddress, N1, N2, H, Fut>
where
    S: Clone + Send + Sync + 'static,
    H: ExclusiveMessageHandler<SessionState = ()> + Send + ListenerErrorHandler + 'static,
    Fut: Future<Output: Display>,
{
    pub async fn run(self) -> Result<()> {
        // Setup Control Server
        let control_listener = start_listener(
            self.pipe_name,
            self.control_handler,
        )
        .context("Setting up control listener")?;

        // Setup Router
        let mut router = Router::new();

        for (route, method) in self.routes {
            router = router.route(route, method);
        }

        let router = router.with_state(self.state).layer(
            ServiceBuilder::new()
                .layer(CompressionLayer::new())
                .layer(TraceLayer::new_for_http())
                .layer(
                    CorsLayer::new()
                        .allow_methods(self.cors_allowed_methods)
                        .allow_origin(self.cors_allowed_origins),
                )
                .layer(RequireAuthorizationLayer::custom(BearerAuth::new(
                    self.api_token,
                    RegexSet::new(self.public_paths).expect("Parsing open paths for Bearer Auth"),
                ))),
        );

        let startup_msg = std::cell::RefCell::new(String::new());

        // Setup side functionality, such as ctrl_c listener
        let fut = async {
            info!("{}", startup_msg.borrow());
            tokio::select! {
                res = tokio::signal::ctrl_c() => {
                    if let Err(e) = res {
                        error!("Faced the following error while listening for ctrl_c: {:?}", e);
                    } else {
                        warn!("Ctrl-C received");
                    }
                }
                res = control_listener => {
                    if let Err(e) = res {
                        error!("Faced the following error while joining with the control listener task: {e:?}");
                    }
                }
                msg = self.concurrent_fut => {
                    warn!("{msg}")
                }
            }
        };

        macro_rules! run {
            ($server:expr, $addr:expr) => {
                *startup_msg.borrow_mut() = format!("Binded to {}", $addr);
                $server
                    .serve(router.into_make_service())
                    .with_graceful_shutdown(fut)
                    .await
                    .context("Running the web server")?;
            };
        }

        // Setup Server
        match self.bind_address {
            #[cfg(unix)]
            BindAddress::Local(addr) => {
                let listener = tokio::net::UnixListener::bind(&addr)
                    .map_err(Into::<Error>::into)
                    .context("Binding to local address")?;
                let stream = tokio_stream::wrappers::UnixListenerStream::new(listener);
                let acceptor = hyper::server::accept::from_stream(stream);
                run!(Server::builder(acceptor), addr);
            }
            #[cfg(not(unix))]
            BindAddress::Local(_) => {
                return Err(Error::msg("Local Sockets are only supported on Unix"))
            }
            BindAddress::Network(addr) => {
                if let Some(identity) = self.https_identity {
                    if addr.port() != 443 {
                        warn!("Serving HTTPS on a different port than 443")
                    }
                    run!(
                        Server::builder(
                            TlsAcceptor::new(identity, &addr).context("Initializing https")?
                        ),
                        addr
                    );
                } else {
                    run!(Server::bind(&addr), addr);
                }
            }
            BindAddress::HTTP(addr) => {
                if let Some(identity) = self.https_identity {
                    let addr = SocketAddr::new(addr, 443);
                    run!(
                        Server::builder(
                            TlsAcceptor::new(identity, &addr).context("Initializing https")?
                        ),
                        addr
                    );
                } else {
                    let addr = SocketAddr::new(addr, 80);
                    run!(Server::bind(&addr), addr);
                }
            }
        };

        Ok(())
    }
}
