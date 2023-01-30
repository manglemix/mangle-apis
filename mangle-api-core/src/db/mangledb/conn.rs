use std::{sync::Arc, net::SocketAddr};

use axum::extract::{State, ConnectInfo};
use bincode::{deserialize, serialize};
use hyper::StatusCode;
use log::error;
use reqwest::{Url, Client, ClientBuilder};
use serde::{Serialize, Deserialize};
use tokio::spawn;

use super::{MangleDB, error::WriteError};


#[derive(Serialize, Deserialize)]
pub struct OwnedWriteRequest {
    namespace: String,
    name: String,
    data: Vec<u8>
}


#[derive(Clone)]
pub struct SiblingConnections {
    sibling_addresses: Arc<Vec<Url>>,
    client: Client
}


impl SiblingConnections {
    pub fn new(sibling_addresses: Vec<Url>) -> reqwest::Result<Self> {
        Ok(Self {
            sibling_addresses: Arc::new(sibling_addresses),
            client: ClientBuilder::new()
                .brotli(true)
                .build()?
        })
    }

    pub(crate) fn propagate_owned_write(&self, namespace: String, name: String, data: Vec<u8>) {
        let data = serialize(&OwnedWriteRequest {
            namespace: namespace.clone(),
            name: name.clone(),
            data
        }).expect("OwnedWriteRequest to serialize");

        for url in self.sibling_addresses.iter() {
            let fut = self.client
                .post(url.clone())
                .body(data.clone())
                .send();
            
            let cloned_url = url.clone();
            let cloned_namespace = namespace.clone();
            let cloned_name = name.clone();

            spawn(async move {
                if let Err(e) = fut.await {
                    error!(
                        target: "mangledb",
                        "Could not propagate write request to sibling: {cloned_url}, {cloned_namespace}::{cloned_name}, error: {e:?}"
                    );
                }
            });
        }
    }
}


pub fn remote_owner_write(ConnectInfo(sender): ConnectInfo<SocketAddr>, State(db): State<MangleDB>, data: Vec<u8>) -> (StatusCode, ()) {
    let req: OwnedWriteRequest = match deserialize(data.as_slice()) {
        Ok(x) => x,
        Err(_) => {
            return (StatusCode::INTERNAL_SERVER_ERROR, ())
        }
    };

    if db.is_record_owned(&req.namespace, &req.name) {
        error!(target: "mangledb", "{sender} sent write request to data we own: {}::{}", req.namespace, req.name);
        return (StatusCode::BAD_REQUEST, ())
    }

    match db.write_record(&req.namespace, req.name, req.data) {
        Ok(()) => { (StatusCode::OK, ()) }
        Err(e) => {
            match e {
                WriteError::BeginTransactionError(_) => {}
                WriteError::SerializeError(_) => {}
                WriteError::InsertError(_) => {}
                WriteError::PrepareError(_) => {}
                WriteError::UnrecognizedNamespace => {}
            }
            (StatusCode::INTERNAL_SERVER_ERROR, ())
        }
    }
}
