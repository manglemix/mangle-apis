use std::{net::SocketAddr, ops::Deref, sync::Arc};

use anyhow::Error;
use bimap::BiMap;
use serde::{de::DeserializeOwned, Serialize};
// use parking_lot::Mutex;
use bincode::{deserialize, serialize};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    spawn,
};
#[cfg(not(debug_assertions))]
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
    #[cfg(not(debug_assertions))]
    tls_builder: TlsConnectorWrapper,
    // message_receiver: broadcast::Receiver<(&'static str, T)>,
    // connections: Mutex<HashMap<&'static str, TlsStream<TcpStream>>>,
    network_port: u16,
}

#[derive(Clone)]
pub struct Node<T: NetworkMessageSet> {
    inner: Arc<NodeImpl<T>>,
}

impl<T: NetworkMessageSet> Node<T> {
    pub async fn new(
        sibling_domains: impl IntoIterator<Item = (String, SocketAddr)>,
        network_port: u16,
        #[cfg(not(debug_assertions))] identity: Identity,
    ) -> anyhow::Result<Self> {
        // let (task_message_sender, message_receiver) = broadcast::channel(16);

        let inner2 = Arc::new(NodeImpl {
            message_router: T::MessageRouter::new(),
            #[cfg(not(debug_assertions))]
            tls_builder: TlsConnectorWrapper::from(TlsConnector::builder().build()?),

            // message_receiver,
            // connections: Mutex::new(HashMap::with_capacity(sibling_domains.len())),
            sibling_domains: sibling_domains
                .into_iter()
                .map(|(domain, addr)| unsafe { (Arc::from(Box::from_raw(domain.leak())), addr) })
                .collect(),
            network_port,
        });

        let inner = inner2.clone();
        #[cfg(not(debug_assertions))]
        let tls_acceptor = TlsAcceptorWrapper::from(TlsAcceptor::new(identity)?);
        let acceptor = TcpListener::bind(("0.0.0.0", network_port)).await?;

        spawn(async move {
            loop {
                let Ok((stream, addr)) = acceptor.accept().await else { continue };

                let inner2 = inner.clone();
                #[cfg(not(debug_assertions))]
                let tls_acceptor2 = tls_acceptor.clone();
                // let task_message_sender2 = task_message_sender.clone();

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
                    if stream.read_to_end(&mut buf).await.is_err() {
                        return;
                    }
                    let Ok(msg) = deserialize(buf.as_slice()) else {
                        // ERROR
                        return
                    };

                    if !T::MessageRouter::route_message(
                        &inner2.message_router,
                        connection_domain,
                        msg,
                    ) {
                        return;
                    }
                    // if !inner2.message_router.route_message(connection_domain, msg) {
                    //     return
                    // }
                });
            }
        });

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
        let connection = TcpStream::connect((domain, self.inner.network_port)).await?;
        #[cfg(debug_assertions)]
        let mut connection = connection;
        #[cfg(not(debug_assertions))]
        let mut connection = self.inner.tls_builder.connect(domain, connection).await?;
        connection
            .write_all(message.as_slice())
            .await
            .map_err(Into::into)
    }

    pub async fn broadcast_message(&self, message: &T) -> Vec<(&str, Error)> {
        let message = serialize(message).expect("message to serialize");
        let mut results = vec![];

        for domain in self.inner.sibling_domains.left_values() {
            let connection =
                match TcpStream::connect((domain.deref(), self.inner.network_port)).await {
                    Ok(x) => x,
                    Err(e) => {
                        results.push((domain.deref(), e.into()));
                        continue;
                    }
                };
            #[cfg(debug_assertions)]
            let mut connection = connection;
            #[cfg(not(debug_assertions))]
            let mut connection = match self.inner.tls_builder.connect(domain, connection).await {
                Ok(x) => x,
                Err(e) => {
                    results.push((domain.as_str(), e.into()));
                    continue;
                }
            };
            match connection.write_all(message.as_slice()).await {
                Ok(_) => {}
                Err(e) => results.push((domain.deref(), e.into())),
            }
        }

        results
    }
}
