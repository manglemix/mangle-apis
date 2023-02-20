use std::{sync::Arc, net::SocketAddr, mem::transmute};

use anyhow::Error;
use bimap::BiMap;
use serde::{de::DeserializeOwned, Serialize};
// use parking_lot::Mutex;
use tokio::{spawn, net::{TcpStream, TcpListener}, sync::{broadcast}, io::{AsyncWriteExt, AsyncReadExt}};
#[cfg(not(debug_assertions))]
use tokio_native_tls::{TlsConnector as TlsConnectorWrapper, TlsAcceptor as TlsAcceptorWrapper, native_tls::{TlsAcceptor, TlsConnector, Identity}};
use bincode::{deserialize, serialize};


struct NodeImpl<T> {
    sibling_domains: BiMap<String, SocketAddr>,
    #[cfg(not(debug_assertions))]
    tls_builder: TlsConnectorWrapper,
    message_receiver: broadcast::Receiver<(&'static str, T)>,
    // connections: Mutex<HashMap<&'static str, TlsStream<TcpStream>>>,
    network_port: u16
}


#[derive(Clone)]
pub struct Node<T: Clone + DeserializeOwned + Serialize + Send + 'static> {
    inner: Arc<NodeImpl<T>>
}


impl<T: Clone + DeserializeOwned + Serialize + Send + 'static> Node<T> {
    pub async fn new(
        sibling_domains: BiMap<String, SocketAddr>,
        network_port: u16,
        #[cfg(not(debug_assertions))]
        identity: Identity
    ) -> anyhow::Result<Self> {
        let (task_message_sender, message_receiver) = broadcast::channel(16);

        let inner2 = Arc::new(NodeImpl {
            #[cfg(not(debug_assertions))]
            tls_builder: TlsConnectorWrapper::from(
                TlsConnector::builder()
                    .build()?
            ),
            message_receiver,
            // connections: Mutex::new(HashMap::with_capacity(sibling_domains.len())),
            sibling_domains,
            network_port
        });

        let inner = inner2.clone();
        #[cfg(not(debug_assertions))]
        let tls_acceptor = TlsAcceptorWrapper::from(
            TlsAcceptor::new(identity)?
        );
        let acceptor = TcpListener::bind(("0.0.0.0", network_port)).await?;
        
        spawn(async move {
            loop {
                let Ok((stream, addr)) = acceptor.accept().await else { continue };

                let inner2 = inner.clone();
                #[cfg(not(debug_assertions))]
                let tls_acceptor2 = tls_acceptor.clone();
                let task_message_sender2 = task_message_sender.clone();

                spawn(async move {
                    let Some(connection_domain) = inner2.sibling_domains.get_by_right(&addr) else {
                        // WARN
                        return
                    };
    
                    #[cfg(debug_assertions)]
                    let mut stream = stream;
                    #[cfg(not(debug_assertions))]
                    let Ok(mut stream) = tls_acceptor2.accept(stream).await else { return };
                    let mut buf = Vec::new();
                    if stream.read_to_end(&mut buf).await.is_err() { return }
                    let Ok(msg) = deserialize(buf.as_slice()) else {
                        // ERROR
                        return
                    };
    
                    if task_message_sender2.send((
                        unsafe { transmute(connection_domain.as_str()) },
                        msg
                    )).is_err() {
                        return
                    }
                });
            }
        });

        Ok(Self {
            inner: inner2
        })
    }

    pub async fn get_message(&self) -> Option<(&str, T)> {
        self.inner.message_receiver.resubscribe().recv().await.ok()
    }

    pub async fn send_message(&self, domain: &str, message: &T) -> Result<(), Error> {
        let message = serialize(message).expect("message to serialize");
        if !self.inner.sibling_domains.contains_left(domain) {
            return Err(Error::msg(format!("{domain} is not a sibling")))
        }
        let connection = TcpStream::connect((domain, self.inner.network_port)).await?;
        #[cfg(debug_assertions)]
        let mut connection = connection;
        #[cfg(not(debug_assertions))]
        let mut connection = self.inner.tls_builder.connect(domain, connection).await?;
        connection.write_all(message.as_slice()).await.map_err(Into::into)
    }

    pub async fn broadcast_message(&self, message: &T) -> Vec<(&str, Error)> {
        let message = serialize(message).expect("message to serialize");
        let mut results = vec![];

        for domain in self.inner.sibling_domains.left_values() {
            let connection = match TcpStream::connect((domain.as_str(), self.inner.network_port)).await {
                Ok(x) => x,
                Err(e) => {
                    results.push((domain.as_str(), e.into()));
                    continue
                }
            };
            #[cfg(debug_assertions)]
            let mut connection = connection;
            #[cfg(not(debug_assertions))]
            let mut connection = match self.inner.tls_builder.connect(domain, connection).await {
                Ok(x) => x,
                Err(e) => {
                    results.push((domain.as_str(), e.into()));
                    continue
                }
            };
            match connection.write_all(message.as_slice()).await {
                Ok(_) => {}
                Err(e) => results.push((domain.as_str(), e.into()))
            }
        }

        results
    }
}