use std::{
    ops::{Deref, DerefMut},
    sync::Arc,
};

use parking_lot::{lock_api::MutexGuard, Mutex, RawMutex};
use redis::{
    cluster::{ClusterClient, ClusterClientBuilder, ClusterConnection},
    RedisResult,
};

pub struct RedisConnection<'a> {
    lock: MutexGuard<'a, RawMutex, Option<ClusterConnection>>,
    ptr: *mut ClusterConnection,
}

impl<'a> Deref for RedisConnection<'a> {
    type Target = ClusterConnection;

    fn deref(&self) -> &Self::Target {
        unsafe { &*self.ptr }
    }
}

impl<'a> DerefMut for RedisConnection<'a> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        unsafe { &mut *self.ptr }
    }
}

impl<'a> RedisConnection<'a> {
    pub fn invalidate(mut self) {
        *self.lock = None;
    }
}

#[derive(Clone)]
pub struct RedisClient {
    client: ClusterClient,
    connection: Arc<Mutex<Option<ClusterConnection>>>,
}

impl RedisClient {
    pub fn new(nodes: Vec<String>, _username: String, _password: String) -> RedisResult<Self> {
        ClusterClientBuilder::new(nodes)
            // .password(password)
            // .username(username)
            .build()
            .map(|client| RedisClient {
                client,
                connection: Default::default(),
            })
    }

    pub fn get_connection(&self) -> RedisResult<RedisConnection<'_>> {
        let mut lock = self.connection.lock();
        if lock.is_none() {
            let connection = self.client.get_connection()?;
            *lock = Some(connection);
        }

        let ptr: *mut _ = lock.as_mut().unwrap();

        Ok(RedisConnection { lock, ptr })
    }
}
