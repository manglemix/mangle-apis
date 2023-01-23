use std::sync::{Arc};

use mangle_api_core::parking_lot::{Mutex, lock_api::MutexGuard, RawMutex};
use redis::{cluster::{ClusterClient, ClusterClientBuilder, ClusterConnection}, RedisResult};


#[derive(Clone)]
pub struct DBClient {
    client: ClusterClient,
    connection: Arc<Mutex<Option<ClusterConnection>>>
}


impl DBClient {
    pub fn new(nodes: Vec<String>, username: String, password: String) -> RedisResult<Self>{
        ClusterClientBuilder::new(nodes)
            .password(password)
            .username(username)
            .build()
            .map(|client| DBClient {
                client,
                connection: Default::default()
            })
    }

    fn get_connection(&self) -> RedisResult<MutexGuard<'_, RawMutex, Option<ClusterConnection>>> {
        let mut lock = self.connection.lock();
        if lock.is_none() {
            let connection = self.client.get_connection()?;
            *lock = Some(connection);
        }
        Ok(lock)
    }


}
