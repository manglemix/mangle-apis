use mangle_api_core::log::error;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const GET_TOURNAMENT_ERR_DURATION: Duration = Duration::from_secs(1);

struct TournamentImpl {
    start_time: SystemTime,
}

#[derive(Clone)]
pub struct Tournament {
    inner: Arc<TournamentImpl>,
}

impl Tournament {
    pub fn new(start_time: Duration) -> Self {
        Self {
            inner: Arc::new(TournamentImpl {
                start_time: UNIX_EPOCH + start_time,
            }),
        }
    }

    pub fn get_tournament_week(&self) -> Option<u64> {
        let now = Instant::now();

        let elapsed = loop {
            let Ok(elapsed) = self.inner.start_time.elapsed() else {
                if now.elapsed() >= GET_TOURNAMENT_ERR_DURATION {
                    error!(target: "tournament", "Could not calculate tournament week");
                    return None
                }
                continue
            };
            break elapsed;
        };

        Some(elapsed.as_secs() / 3600 / 24 / 7)
    }
}
