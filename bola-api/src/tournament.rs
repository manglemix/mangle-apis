use mangle_api_core::log::error;
use serde::Serialize;
use std::{
    sync::Arc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

const GET_TOURNAMENT_ERR_DURATION: Duration = Duration::from_secs(1);

struct TournamentImpl {
    start_time: SystemTime,
    start_time_duration: Duration
}

#[derive(Clone)]
pub struct Tournament {
    inner: Arc<TournamentImpl>,
}


#[derive(Serialize)]
pub struct TournamentData {
    pub week: u64,
    start_time: u64,
    end_time: u64
}


impl Tournament {
    pub fn new(start_time: Duration) -> Self {
        Self {
            inner: Arc::new(TournamentImpl {
                start_time: UNIX_EPOCH + start_time,
                start_time_duration: start_time
            }),
        }
    }

    pub fn get_tournament_week(&self) -> Option<TournamentData> {
        let now = Instant::now();

        let elapsed = loop {
            let Ok(elapsed) = self.inner.start_time.elapsed() else {
                if now.elapsed() >= GET_TOURNAMENT_ERR_DURATION {
                    error!(target: "tournament", "Could not calculate tournament week");
                    return None
                }
                continue
            };
            break elapsed.as_secs();
        };

        let week = elapsed / 3600 / 24 / 7;
        let start_time = self.inner.start_time_duration.as_secs();

        Some(TournamentData {
            week,
            start_time: week * 3600 * 24 * 7 + start_time,
            end_time: (week + 1) * 3600 * 24 * 7 + start_time
        })
    }
}
