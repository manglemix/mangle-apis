use std::{
    net::SocketAddr,
    pin::Pin,
    sync::Arc,
    task::{self, Poll},
};

use anyhow::Error;
use futures::future::BoxFuture;
use hyper::server::{
    accept::Accept,
    conn::{AddrIncoming, AddrStream},
};
use log::error;
use tokio_native_tls::{
    native_tls::{Identity, TlsAcceptor as InnerTlsAcceptor},
    TlsAcceptor as TlsAcceptorWrapper, TlsStream,
};

pub struct TlsAcceptor<'a> {
    incoming: AddrIncoming,
    acceptor_loop: Option<BoxFuture<'a, Result<TlsStream<AddrStream>, Error>>>,
    tls_acceptor: Arc<TlsAcceptorWrapper>,
}

impl<'a> TlsAcceptor<'a> {
    pub fn new(identity: Identity, addr: &SocketAddr) -> anyhow::Result<Self> {
        Ok(Self {
            incoming: AddrIncoming::bind(addr)?,
            acceptor_loop: None,
            tls_acceptor: Arc::new(TlsAcceptorWrapper::from(InnerTlsAcceptor::new(identity)?)),
        })
    }
}

impl<'a> Accept for TlsAcceptor<'a> {
    type Conn = TlsStream<AddrStream>;

    type Error = !;

    fn poll_accept(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        if let Some(acceptor_loop) = &mut self.acceptor_loop {
            let Poll::Ready(result) = acceptor_loop.as_mut().poll(cx) else {
                return Poll::Pending
            };

            self.acceptor_loop = None;

            match result {
                Ok(stream) => return Poll::Ready(Some(Ok(stream))),
                Err(e) => {
                    error!(target: "routing", "Error authenticating connection: {e:?}");
                }
            }
        }

        let stream = match Pin::new(&mut self.incoming).poll_accept(cx) {
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => {
                error!(target: "routing", "Error accepting connection: {e:?}");
                return Poll::Pending;
            }
            Poll::Ready(Some(Ok(x))) => x,
            Poll::Pending => return Poll::Pending,
        };

        let tls = self.tls_acceptor.clone();
        self.acceptor_loop = Some(Box::pin(async move {
            tls.accept(stream).await.map_err(Into::into)
        }));

        self.poll_accept(cx)
    }
}
