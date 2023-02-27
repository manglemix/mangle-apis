#![feature(string_leak)]

use axum::extract::connect_info::Connected;
use axum::http::HeaderValue;
use axum::routing::MethodRouter;
use axum::{Router, Server};

pub mod auth;
// pub mod sync;
pub mod distributed;
pub mod neo_api;
pub mod ws;

#[cfg(any(feature = "redis"))]
pub mod db;

use anyhow::{Context, Error, Result};
use clap::builder::IntoResettable;
use clap::{arg, Command};
use derive_more::From;
use hyper::server::conn::AddrStream;
use mangle_detached_console::{send_message, ConsoleSendError};

use fern::{log_file, Dispatch};
use log::{error, info, warn, LevelFilter};
use mangle_detached_console::ConsoleServer;
use parking_lot::Mutex;
use regex::{Regex, RegexSet};
use serde::de::DeserializeOwned;
use std::env;
use std::ffi::{OsStr, OsString};
use std::fs::read_to_string;
use std::net::SocketAddr;
use std::sync::Arc;
use toml::from_str;
use tower::ServiceBuilder;
use tower_http::cors::{AllowMethods, AllowOrigin};
use tower_http::{
    auth::RequireAuthorizationLayer, compression::CompressionLayer, cors::CorsLayer,
    trace::TraceLayer,
};

use auth::bearer::BearerAuth;

pub use bimap;
pub use log;
pub use parking_lot;
#[cfg(any(feature = "redis"))]
pub use redis;
pub use regex;
pub use serde_json;
pub use toml;
pub use tower_http;

mod log_targets {
    pub const SUSPICIOUS_SECURITY: &str = "suspicious_security";
}

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

#[derive(From)]
pub enum BindAddress {
    Local(String),
    Network(SocketAddr),
}

pub struct BaseConfig<A: Into<AllowMethods>, B: Into<AllowOrigin>, C: Into<BindAddress>> {
    pub stderr_log_path: String,
    pub routing_log_path: String,
    pub suspicious_security_log_path: String,
    pub cors_allowed_methods: A,
    pub cors_allowed_origins: B,
    pub api_token: HeaderValue,
    pub bind_address: C,
}

pub fn get_pipe_name(pipe_name_env_var: &'static str, default_pipe_name: &'static str) -> OsString {
    match env::var_os(pipe_name_env_var) {
        Some(x) => x,
        None => default_pipe_name.into(),
    }
}

pub async fn pre_matches<Config: DeserializeOwned>(
    app: Command,
    pipe_name: &OsStr,
) -> Result<Option<Config>> {
    let args: Vec<String> = env::args().collect();
    let matches = app.get_matches_from(args.clone());

    let config_path;

    match matches.subcommand() {
        Some(("start", matches)) => match send_message(
            pipe_name,
            format!("{} status", env::current_exe()?.display()),
        )
        .await
        {
            Ok(msg) => {
                return Err(Error::msg(format!(
                    "A server has already started up. Retrieved their status: {msg}"
                )))
            }

            Err(e) => match e {
                ConsoleSendError::NotFound => {
                    // Do nothing and move on, there is no server
                    config_path = matches
                        .get_one("config_path")
                        .cloned()
                        .unwrap_or("configs.toml".to_string());
                }
                e => return Err(e).context("Verifying if server is already active"),
            },
        },

        None => {
            return Err(Error::msg(
                "You need to type a command as an argument! Use -h for more information",
            ))
        }

        _ => {
            // All subcommands not caught by the match should be sent to the server
            return match send_message(pipe_name, args.join(" ")).await {
                Ok(msg) => {
                    println!("{msg}");
                    Ok(None)
                }
                Err(e) => Err(match e {
                    ConsoleSendError::NotFound => {
                        Error::msg("Could not issue command. The server may not be running")
                    }
                    ConsoleSendError::PermissionDenied => {
                        Error::msg("Could not issue command. You may not have adequate permissions")
                    }
                    _ => Error::msg(format!(
                        "Faced the following error while trying to issue the command: {e:?}"
                    )),
                }),
            };
        }
    }

    macro_rules! err {
        () => {
            |e| {
                Into::<anyhow::Error>::into(e)
                    .context(format!("Reading configuration file: {config_path}"))
            }
        };
    }

    from_str(&read_to_string(&config_path).map_err(err!())?)
        .map_err(err!())
        .map(Option::Some)
}

pub async fn start_api<State, const N1: usize, const N2: usize, A, B, C, ConnectInfo>(
    state: State,
    app: Command,
    pipe_name: OsString,
    config: BaseConfig<A, B, C>,
    public_paths: [&'static str; N1],
    routes: [(&'static str, MethodRouter<State>); N2],
) -> Result<()>
where
    State: Clone + Send + Sync + 'static,
    A: Into<AllowMethods>,
    B: Into<AllowOrigin>,
    C: Into<BindAddress>,
    ConnectInfo: for<'a> Connected<&'a AddrStream>,
{
    // Setup logger
    static CRITICAL_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Info);
    static STDERR_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Trace);
    static ROUTING_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Trace);

    const ROUTING_REGEX_RAW: &str = "^(tower_http::trace|hyper::proto|mio|tracing)";

    let routing_regex = Regex::new(ROUTING_REGEX_RAW).unwrap();

    let non_stderr = Arc::new(
        RegexSet::new([
            ROUTING_REGEX_RAW.to_string(),
            format!("^{}", log_targets::SUSPICIOUS_SECURITY),
        ])
        .unwrap(),
    );

    let non_stderr2 = non_stderr.clone();

    Dispatch::new()
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
                    log_file(&config.stderr_log_path)
                        .context(format!("Opening {:?}", config.stderr_log_path))?,
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
                    log_file(&config.routing_log_path)
                        .context(format!("Opening {:?}", config.routing_log_path))?,
                ),
        )
        // Suspicious security to file (maybe more?)
        .chain(
            Dispatch::new()
                .filter(|metadata| {
                    metadata
                        .target()
                        .starts_with(log_targets::SUSPICIOUS_SECURITY)
                })
                .chain(
                    log_file(&config.suspicious_security_log_path)
                        .context(format!("Opening {:?}", config.suspicious_security_log_path))?,
                ),
        )
        .apply()?;

    // Setup Console Server
    let mut console_server = ConsoleServer::bind(&pipe_name).context("Starting ConsoleServer")?;

    // Setup Router
    let mut router = Router::new();

    for (route, method) in routes {
        router = router.route(route, method);
    }

    let router = router.with_state(state).layer(
        ServiceBuilder::new()
            .layer(CompressionLayer::new())
            .layer(TraceLayer::new_for_http())
            .layer(
                CorsLayer::new()
                    .allow_methods(config.cors_allowed_methods)
                    .allow_origin(config.cors_allowed_origins),
            )
            .layer(RequireAuthorizationLayer::custom(BearerAuth::new(
                config.api_token,
                RegexSet::new(public_paths).expect("Parsing open paths for Bearer Auth"),
            ))),
    );

    // Setup side functionality
    let mut final_event = None;
    let fut = async {
        tokio::select! {
            res = tokio::signal::ctrl_c() => {
                if let Err(e) = res {
                    error!(target: "console_server", "Faced the following error while listening for ctrl_c: {:?}", e);
                } else {
                    warn!(target: "console_server", "Ctrl-C received");
                }
            }
            () = async {
                loop {
                    let mut event = match console_server.accept().await {
                        Ok(x) => x,
                        Err(e) => {
                            error!(target: "console_server", "Received IOError while listening on ConsoleServer {e:?}");
                            continue
                        }
                    };
                    let message = event.take_message().unwrap();

                    let matches = match app.clone().try_get_matches_from(message.split_whitespace()) {
                        Ok(x) => x,
                        Err(e) => {
                            error!(target: "console_server", "Failed to parse client console message {message}. Error: {e:?}");
                            continue
                        }
                    };

                    macro_rules! write_all {
                        ($msg: expr) => {
                            match event.write_all($msg).await {
                                Ok(()) => {}
                                Err(e) => {
                                    error!(target: "console_server", "Failed to respond to client console {e:?}");
                                    continue
                                }
                            }
                        }
                    }

                    match matches.subcommand().unwrap() {
                        ("log_level", matches) => match matches.get_one::<String>("new_level") {
                            Some(new_level) => {
                                let new_level = match new_level.parse() {
                                    Ok(x) => x,
                                    Err(e) => {
                                        error!(target: "console_server", "Failed to parse new_level: {e:?}");
                                        write_all!(format!("Failed to parse new_level: {e:?}").as_str());
                                        continue
                                    }
                                };

                                match matches.get_one::<String>("target").unwrap().as_str() {
                                    "stderr" => *STDERR_LOG_LEVEL.lock() = new_level,
                                    "routing" => *ROUTING_LOG_LEVEL.lock() = new_level,
                                    _ => write_all!("Unrecognized target")
                                }
                            }
                            None => match matches.get_one::<String>("target").unwrap().as_str() {
                                "stderr" => write_all!(STDERR_LOG_LEVEL.lock().to_string().to_lowercase().as_str()),
                                "routing" => write_all!(ROUTING_LOG_LEVEL.lock().to_string().to_lowercase().as_str()),
                                _ => write_all!("Unrecognized target")
                            }
                        }
                        ("status", _) => write_all!("Server is good!"),
                        ("stop", _) => {
                            final_event = Some(event);
                            warn!("Stop command issued");
                            break
                        }
                        (cmd, _) => {
                            error!(target: "console_server", "Received the following unrecognized command from client console: {cmd}");
                            write_all!("Unrecognized command: {cmd}")
                        }
                    }
                }
            } => {}
        }
    };

    macro_rules! run {
        ($server: expr, $addr: expr) => {
            let server = $server
                .serve(router.into_make_service_with_connect_info::<ConnectInfo>())
                .with_graceful_shutdown(fut);

            info!("Binded to {:?}", $addr);

            // Only print criticals from now on
            *CRITICAL_LOG_LEVEL.lock() = LevelFilter::Error;

            // Free some memory
            drop(pipe_name);

            server
                .await
                .map_err(Into::<Error>::into)
                .context("Running the web server")?;
        };
    }

    // Setup Server
    match config.bind_address.into() {
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
            run!(Server::bind(&addr), addr);
        }
    };

    if let Some(mut event) = final_event {
        event.write_all("Server stopped successfully").await?
    }

    Ok(())
}
