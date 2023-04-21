use std::{net::SocketAddr, sync::Arc};

use anyhow::Error;
use bimap::BiMap;
use log::warn;
use messagist::{bin::BinaryMessageStream, ExclusiveMessageHandler, MessageStream};
use serde::Serialize;
use tokio::{
    net::{TcpListener, TcpStream},
    spawn,
    task::JoinHandle,
};
use tokio_native_tls::{
    native_tls::{Identity, TlsAcceptor, TlsConnector},
    TlsAcceptor as TlsAcceptorWrapper, TlsConnector as TlsConnectorWrapper,
};

pub struct ServerName(pub Arc<str>);

pub struct Node<H>
where
    H: ExclusiveMessageHandler<SessionState = ServerName> + Clone + Send + Sync + 'static,
{
    sibling_domains: Arc<BiMap<Arc<str>, SocketAddr>>,
    tls_builder: Option<TlsConnectorWrapper>,
    network_port: u16,
    task_handle: JoinHandle<()>,
    handler: H,
}

impl<H> Drop for Node<H>
where
    H: ExclusiveMessageHandler<SessionState = ServerName> + Clone + Send + Sync + 'static,
{
    fn drop(&mut self) {
        self.task_handle.abort();
    }
}

impl<H> Node<H>
where
    H: ExclusiveMessageHandler<SessionState = ServerName> + Clone + Send + Sync + 'static,
{
    pub async fn new(
        sibling_domains: impl IntoIterator<Item = (String, SocketAddr)>,
        network_port: u16,
        identity: Option<Identity>,
        handler: H,
    ) -> anyhow::Result<Self> {
        let sibling_domains = Arc::new(
            sibling_domains
                .into_iter()
                .map(|(domain, addr)| (Arc::from(domain.into_boxed_str()), addr))
                .collect::<BiMap<_, _>>(),
        );

        let sibling_domains2 = sibling_domains.clone();

        let tls_acceptor;
        let tls_builder;

        if let Some(identity) = identity {
            tls_builder = Some(TlsConnectorWrapper::from(TlsConnector::builder().build()?));
            tls_acceptor = Some(TlsAcceptorWrapper::from(TlsAcceptor::new(identity)?))
        } else {
            tls_builder = None;
            tls_acceptor = None;
        };
        let acceptor = TcpListener::bind(("0.0.0.0", network_port)).await?;
        let handler2 = handler.clone();

        let task_handle = spawn(async move {
            loop {
                let Ok((stream, addr)) = acceptor.accept().await else { continue };

                let Some(connection_domain) = sibling_domains2.get_by_right(&addr).cloned() else {
                    warn!(target: "security", "Got attempted connection from {addr}");
                    return
                };

                let server_name = ServerName(connection_domain);

                let mut handler2 = handler2.clone();
                let tls_acceptor2 = tls_acceptor.clone();

                spawn(async move {
                    match &tls_acceptor2 {
                        Some(tls_acceptor) => {
                            let Ok(stream) = tls_acceptor.accept(stream).await else { return };
                            handler2.handle(BinaryMessageStream::from(stream), server_name)
                        }
                        None => handler2.handle(BinaryMessageStream::from(stream), server_name),
                    };
                });
            }
        });

        Ok(Self {
            tls_builder,
            sibling_domains,
            network_port,
            task_handle,
            handler,
        })
    }

    pub async fn send_message<T>(&self, domain: &str, message: T) -> Result<(), Error>
    where
        T: Serialize + Send + Sync,
    {
        if !self.sibling_domains.contains_left(domain) {
            return Err(Error::msg(format!("{domain} is not a sibling")));
        }

        let connection = TcpStream::connect((domain, self.network_port)).await?;

        match &self.tls_builder {
            Some(tls_builder) => {
                BinaryMessageStream::from(tls_builder.connect(domain, connection).await?)
                    .send_message(message)
                    .await
                    .map_err(Into::into)
            }
            None => BinaryMessageStream::from(connection)
                .send_message(message)
                .await
                .map_err(Into::into),
        }
    }

    pub async fn broadcast_message<T>(&self, message: T) -> Vec<(String, Error)>
    where
        T: Serialize + Send + Sync,
    {
        let mut results = vec![];
        let domains = self
            .sibling_domains
            .left_values()
            .map(ToString::to_string)
            .collect::<Vec<_>>();

        for domain in domains {
            let connection = match TcpStream::connect((domain.as_str(), self.network_port)).await {
                Ok(x) => x,
                Err(e) => {
                    results.push((domain, e.into()));
                    continue;
                }
            };
            match &self.tls_builder {
                Some(tls_builder) => {
                    match tls_builder.connect(&domain, connection).await {
                        Ok(connection) => {
                            let mut connection = BinaryMessageStream::from(connection);
                            if let Err(e) = connection.send_message(&message).await {
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
                    if let Err(e) = BinaryMessageStream::from(connection)
                        .send_message(&message)
                        .await
                    {
                        results.push((domain, e.into()));
                    }
                }
            }
        }

        results
    }

    pub fn get_handler(&self) -> &H {
        &self.handler
    }

    pub fn get_mut_handler(&mut self) -> &mut H {
        &mut self.handler
    }
}
