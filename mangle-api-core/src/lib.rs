#![feature(iterator_try_collect)]
#![feature(async_closure)]

use axum::http::HeaderValue;
use axum::routing::MethodRouter;
use axum::Router;

pub mod auth;

use anyhow::{Result, Context, Error};
use clap::builder::IntoResettable;
use clap::{Command, arg};
use mangle_detached_console::{send_message, ConsoleSendError};

use mangle_detached_console::ConsoleServer;
use parking_lot::Mutex;
use fern::{Dispatch, log_file};
use log::{LevelFilter, info, warn, error};
use serde::de::DeserializeOwned;
use toml::from_str;
use tower::ServiceBuilder;
use tower_http::cors::{AllowMethods, AllowOrigin};
use tower_http::{
    cors::CorsLayer,
    compression::CompressionLayer,
    trace::TraceLayer,
    auth::RequireAuthorizationLayer
};
use std::ffi::{OsString, OsStr};
use std::fs::read_to_string;
use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use regex::{RegexSet, Regex};
use std::env;

use auth::bearer::BearerAuth;

pub use tokio;
pub use axum;
pub use anyhow;
pub use serde;
pub use toml;
pub use tower_http;


mod log_targets {
    pub const SUSPICIOUS_SECURITY: &str = "suspicious_security";
}


pub fn make_app<const N: usize>(
    name: &'static str,
    version: impl IntoResettable<clap::builder::Str>,
    about: &'static str,
    extra_log_targets: [&'static str; N]
) -> Command {

    Command::new(name)
        .version(version)
        .author("manglemix")
        .about(about)
        .subcommand(
            Command::new("start")
                .about("Starts the web server in the current directory")
                .arg(
                    arg!([config_path] "An optional path to a config file")
                )
        )
        .subcommand(
            Command::new("log_level")
                .about("Sets or gets the log level of a specific log target")
                .arg(
                    arg!(<target> "The logging target to set or get")
                        .value_parser(
                            ["stderr", "routing"]
                                .into_iter()
                                .chain(extra_log_targets)
                                .collect::<Vec<_>>()
                        )
                )
                .arg(
                    arg!([new_level] "If provided, will set the log level for the given target")
                        .value_parser(["off", "error", "warn", "info", "debug", "trace"])
                )
        )
        .subcommand(
            Command::new("status")
                .about("Checks the status of the server")
        )
        .subcommand(
            Command::new("stop")
                .about("Stops the currently running server")
        )
}


pub trait BaseConfig {
    fn get_stderr_log_path(&self) -> Result<&Path>;
    fn get_routing_log_path(&self) -> Result<&Path>;
    fn get_suspicious_security_log_path(&self) -> Result<&Path>;
    fn get_cors_allowed_methods(&self) -> Result<AllowMethods>;
    fn get_cors_allowed_origins(&self) -> Result<AllowOrigin>;
    fn get_api_token(&self) -> Result<HeaderValue>;
    fn get_bind_address(&self) -> Result<SocketAddr>;
}


pub fn get_pipe_name(pipe_name_env_var: &'static str, default_pipe_name: &'static str) -> OsString {
	match env::var_os(pipe_name_env_var) {
		Some(x) => x,
		None => default_pipe_name.into()
	}
}


pub async fn pre_matches<Config: DeserializeOwned>(app: Command, pipe_name: &OsStr) -> Result<Option<Config>> {
    let args: Vec<String> = env::args().collect();
    let matches = app.get_matches_from(args.clone());

    let config_path;

    match matches.subcommand() {
        Some(("start", matches)) => match send_message(
            pipe_name,
            format!("{} status", env::current_exe()?.display())
        ).await {
            Ok(msg) => return Err(Error::msg(format!("A server has already started up. Retrieved their status: {msg}"))),
            
            Err(e) => match e {
                ConsoleSendError::NotFound => {
                    // Do nothing and move on, there is no server
                    config_path = matches
                        .get_one("config_path")
                        .cloned()
                        .unwrap_or("configs.toml".to_string());
                }
                e => return Err(e).context("Verifying if server is already active")
            }
        }

        None => return Err(Error::msg("You need to type a command as an argument! Use -h for more information")),

        _ => {
            // All subcommands not caught by the match should be sent to the server
            return match send_message(pipe_name, args.join(" ")).await {
                Ok(msg) => {
                    println!("{msg}");
                    Ok(None)
                }
                Err(e) => Err(match e {
                    ConsoleSendError::NotFound => Error::msg("Could not issue command. The server may not be running"),
                    ConsoleSendError::PermissionDenied => Error::msg("Could not issue command. You may not have adequate permissions"),
                    _ => Error::msg(format!("Faced the following error while trying to issue the command: {e:?}"))
                })
            }
        }
    }

    from_str(&read_to_string(&config_path)?)
        .map_err(Into::<anyhow::Error>::into)
        .context(format!("Reading configuration file: {config_path}"))
        .map(Option::Some)
}


pub async fn start_api<State, Config, const N1: usize, const N2: usize>(
    state: State,
    app: Command,
    pipe_name: OsString,
    config: Config,
    public_paths: [&'static str; N1],
    routes: [(&'static str, MethodRouter<(), axum::body::Body>); N2]
) -> Result<()>
where 
    State: Clone + Send + Sync + 'static,
    Config: BaseConfig
{
    // Setup logger
    static CRITICAL_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Info);
    static STDERR_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Trace);
    static ROUTING_LOG_LEVEL: Mutex<LevelFilter> = Mutex::new(LevelFilter::Trace);

    const ROUTING_REGEX_RAW: &str = "^(tower_http::trace|hyper::proto|mio|tracing)";

    let routing_regex = Regex::new(ROUTING_REGEX_RAW).unwrap();

    let non_stderr = Arc::new(RegexSet::new([
        ROUTING_REGEX_RAW.to_string(),
        format!("^{}", log_targets::SUSPICIOUS_SECURITY)
    ]).unwrap());

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
                    !non_stderr.is_match(metadata.target()) && metadata.level() <= *CRITICAL_LOG_LEVEL.lock()
                })
                .chain(std::io::stderr())
        )
        // All Stderr to file
        .chain(
            Dispatch::new()
                .filter(move |metadata| {
                    !non_stderr2.is_match(metadata.target()) && metadata.level() <= *STDERR_LOG_LEVEL.lock()
                })
                .chain(
                    log_file(&config.get_stderr_log_path()?)
                        .context(format!("Opening {:?}", config.get_stderr_log_path()))?
                )
        )
        // Routing to file
        .chain(
            Dispatch::new()
                .filter(move |metadata| {
                    routing_regex.is_match(metadata.target()) && metadata.level() <= *ROUTING_LOG_LEVEL.lock()
                })
                .chain(
                    log_file(&config.get_routing_log_path()?)
                        .context(format!("Opening {:?}", config.get_routing_log_path()))?
                )
        )
        // Suspicious security to file (maybe more?)
        .chain(
            Dispatch::new()
                .filter(|metadata| {
                    metadata.target().starts_with(log_targets::SUSPICIOUS_SECURITY)
                })
                .chain(
                    log_file(&config.get_suspicious_security_log_path()?)
                        .context(format!("Opening {:?}", config.get_suspicious_security_log_path()))?
                )
        )
        .apply()?;

    // Setup Console Server
    let mut console_server = ConsoleServer::bind(&pipe_name)
        .context("Starting ConsoleServer")?;

    // Setup Router
    let mut router = Router::new()
        .with_state(state)
        .layer(
            ServiceBuilder::new()
                .layer(CompressionLayer::new())
                .layer(TraceLayer::new_for_http())
                .layer(CorsLayer::new()
                    .allow_methods(config.get_cors_allowed_methods()?)
                    .allow_origin(config.get_cors_allowed_origins()?)
                )
                .layer(RequireAuthorizationLayer::custom(BearerAuth::new(
                    config.get_api_token()?,
                    RegexSet::new(public_paths).expect("Parsing open paths for Bearer Auth")
                )))
        );
    
    for (route, method) in routes {
        router = router.route(route, method);
    }

    // Setup Server
    let addr = config.get_bind_address()?;
    let server = axum::Server::bind(&addr);
    
    info!("Binded to {:?}", addr);

    // Only print criticals from now on
    *CRITICAL_LOG_LEVEL.lock() = LevelFilter::Error;

    // Free some memory
    drop((config, addr, pipe_name));

    // Main tasks
    // 1. Axum server
    // 2. Ctrl-C
    // 3. Console Server
    let mut final_event = None;
    tokio::select!{
        e = server.serve(router.into_make_service()) => {
            if !e.is_err() {
                error!("axum Server closed without error!?")
            }
            e?
        }
        
        e = tokio::signal::ctrl_c() => {
            warn!("Ctrl-C received");
            e?
        },

        () = async {
			loop {
				let mut event = match console_server.accept().await {
					Ok(x) => x,
					Err(e) => {
						error!("Received IOError while listening on ConsoleServer {e:?}");
						continue
					}
				};
				let message = event.take_message();

				let matches = match app.clone().try_get_matches_from(message.split_whitespace()) {
					Ok(x) => x,
					Err(e) => {
						error!("Failed to parse client console message {message}. Error: {e:?}");
						continue
					}
				};

				macro_rules! write_all {
					($msg: expr) => {
						match event.write_all($msg).await {
							Ok(()) => {}
							Err(e) => {
                                error!("Failed to respond to client console {e:?}");
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
                                    error!("Failed to parse new_level: {e:?}");
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
						return
					}
					(cmd, _) => {
						error!("Received the following unrecognized command from client console: {cmd}");
                        write_all!("Unrecognized command: {cmd}")
					}
				}
			}
		} => {}
    }
    
	if let Some(mut event) = final_event {
        event.write_all("Server stopped successfully").await?
	}

    Ok(())
}
