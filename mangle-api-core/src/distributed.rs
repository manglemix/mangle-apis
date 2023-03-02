use std::{mem::take, net::SocketAddr, sync::Arc};

use anyhow::Error;
use bimap::BiMap;
use bincode::{deserialize, serialize};
use log::{error, warn};
use parking_lot::Mutex;
use serde::{de::DeserializeOwned, Serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    spawn,
    task::JoinHandle,
};
use tokio_native_tls::{
    native_tls::{Identity, TlsAcceptor, TlsConnector},
    TlsAcceptor as TlsAcceptorWrapper, TlsConnector as TlsConnectorWrapper,
};

pub trait MessageRouter<T>: Send + Sync {
    fn new() -> Self;
    /// Sends the given message to the right handlers
    ///
    /// Returns true iff it is possible to continue sending messages.
    /// For example, this should return false if it there are no handlers
    /// and it is not handlers for there to be more
    fn route_message(&self, domain: &Arc<str>, message: T) -> bool;
}

pub trait NetworkMessageSet: Sized + DeserializeOwned + Serialize + 'static {
    type MessageRouter: MessageRouter<Self>;
}

struct NodeImpl<T: NetworkMessageSet> {
    sibling_domains: BiMap<Arc<str>, SocketAddr>,
    message_router: T::MessageRouter,
    tls_builder: Option<TlsConnectorWrapper>,
    network_port: u16,
    task_handle: Mutex<Option<JoinHandle<()>>>,
}

impl<T: NetworkMessageSet> Drop for NodeImpl<T> {
    fn drop(&mut self) {
        if let Some(x) = take(self.task_handle.get_mut()) {
            x.abort()
        }
    }
}

#[derive(Clone)]
pub struct Node<T: NetworkMessageSet> {
    inner: Arc<NodeImpl<T>>,
}

impl<T: NetworkMessageSet> Node<T> {
    pub async fn new(
        sibling_domains: impl IntoIterator<Item = (String, SocketAddr)>,
        network_port: u16,
        identity: Option<Identity>,
    ) -> anyhow::Result<Self> {
        let inner2 = Arc::new(NodeImpl {
            message_router: T::MessageRouter::new(),
            tls_builder: if identity.is_some() {
                Some(TlsConnectorWrapper::from(TlsConnector::builder().build()?))
            } else {
                None
            },
            sibling_domains: sibling_domains
                .into_iter()
                .map(|(mut domain, addr)| unsafe {
                    domain.shrink_to_fit();
                    (Arc::from(Box::from_raw(domain.leak())), addr)
                })
                .collect(),
            network_port,
            task_handle: Default::default(),
        });

        let inner = inner2.clone();
        let tls_acceptor = if let Some(identity) = identity {
            Some(TlsAcceptorWrapper::from(TlsAcceptor::new(identity)?))
        } else {
            None
        };
        let acceptor = TcpListener::bind(("0.0.0.0", network_port)).await?;

        *inner2.task_handle.lock() = Some(spawn(async move {
            loop {
                let Ok((mut stream, addr)) = acceptor.accept().await else { continue };

                let inner2 = inner.clone();
                let tls_acceptor2 = tls_acceptor.clone();

                spawn(async move {
                    let Some(connection_domain) = inner2.sibling_domains.get_by_right(&addr) else {
                        warn!(target: "security", "Got attempted connection from {addr}");
                        return
                    };

                    macro_rules! read {
                        ($stream: expr) => {{
                            let mut buf = Vec::new();
                            if $stream.read_to_end(&mut buf).await.is_err() {
                                return;
                            }
                            let Ok(msg) = deserialize(buf.as_slice()) else {
                                                        error!("Received bad message from {addr}");
                                                        return
                                                    };

                            T::MessageRouter::route_message(
                                &inner2.message_router,
                                connection_domain,
                                msg,
                            );
                        }};
                    }

                    match &tls_acceptor2 {
                        Some(tls_acceptor) => {
                            let Ok(mut stream) = tls_acceptor.accept(stream).await else { return };
                            read!(stream)
                        }
                        None => read!(stream),
                    };
                });
            }
        }));

        Ok(Self { inner: inner2 })
    }

    pub fn get_message_router(&self) -> &T::MessageRouter {
        &self.inner.message_router
    }

    pub async fn send_message(&self, domain: &str, message: &T) -> Result<(), Error> {
        let message = serialize(message).expect("message to serialize");
        if !self.inner.sibling_domains.contains_left(domain) {
            return Err(Error::msg(format!("{domain} is not a sibling")));
        }
        let mut connection = TcpStream::connect((domain, self.inner.network_port)).await?;
        match &self.inner.tls_builder {
            Some(tls_builder) => tls_builder
                .connect(domain, connection)
                .await?
                .write_all(message.as_slice())
                .await
                .map_err(Into::into),
            None => connection
                .write_all(message.as_slice())
                .await
                .map_err(Into::into),
        }
    }

    pub async fn broadcast_message(&self, message: &T) -> Vec<(String, Error)> {
        let message = serialize(message).expect("message to serialize");
        let mut results = vec![];
        let domains = self
            .inner
            .sibling_domains
            .left_values()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        for domain in domains {
            let mut connection =
                match TcpStream::connect((domain.as_str(), self.inner.network_port)).await {
                    Ok(x) => x,
                    Err(e) => {
                        results.push((domain, e.into()));
                        continue;
                    }
                };
            match &self.inner.tls_builder {
                Some(tls_builder) => {
                    match tls_builder.connect(&domain, connection).await {
                        Ok(mut connection) => {
                            if let Err(e) = connection.write_all(message.as_slice()).await {
                                results.push((domain, e.into()));
                            }
                        }
                        Err(e) => {
                            results.push((domain, e.into()));
                            continue;
                        }
                    };
                }
                None => {
                    if let Err(e) = connection.write_all(message.as_slice()).await {
                        results.push((domain, e.into()));
                    }
                }
            }
        }

        results
    }
}
