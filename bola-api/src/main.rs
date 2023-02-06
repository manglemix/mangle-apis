use db::DB;
use mangle_api_core::{
    make_app,
    start_api,
    anyhow::{self},
    tokio,
    pre_matches,
    get_pipe_name,
};

mod config;
mod db;

use config::Config;


#[derive(Clone)]
struct GlobalState {
    db_client: DB
}


#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let app = make_app(
        "BolaAPI",
        env!("CARGO_PKG_VERSION"),
        "The API for Bola",
        []
    );

    let pipe_name = get_pipe_name("BOLA_PIPE_NAME", "bola_pipe");

    let Some(config) = pre_matches::<Config>(app.clone(), &pipe_name).await? else {
        return Ok(())
    };

    let state = GlobalState {
        db_client: DB::new().await
    };

    start_api(
        state,
        app,
        pipe_name,
        config,
        [

        ],
        [

        ]
    ).await
}
