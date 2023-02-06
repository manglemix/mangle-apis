use std::{sync::Arc};

use log::error;
use reqwest::{Url, Client, ClientBuilder};
use tokio::spawn;


// #[derive(Serialize, Deserialize)]
// pub struct OwnedWriteRequest {
//     pub(crate) namespace: String,
//     pub(crate) name: String,
//     pub(crate) data: Vec<u8>
// }


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

    pub(crate) fn propagate_message(&self, namespace: &'static str, name: String, data: Vec<u8>) {
        for url in self.sibling_addresses.iter() {
            let fut = self.client
                .post(url.clone())
                .body(data.clone())
                .send();
            
            let cloned_url = url.clone();
            let cloned_name = name.clone();

            spawn(async move {
                if let Err(e) = fut.await {
                    error!(
                        target: "mangledb",
                        "Could not propagate write request to sibling: {cloned_url}, {namespace}::{cloned_name}, error: {e:?}"
                    );
                }
            });
        }
    }
}
