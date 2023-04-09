use std::{
    mem::take,
    net::SocketAddr,
    pin::Pin,
    task::{self, Poll},
};

use anyhow::Error;
use futures::{future::BoxFuture, Future};
use hyper::server::{accept::Accept, conn::AddrIncoming};
use tokio::{
    net::TcpStream,
    sync::mpsc::{channel, error::{TrySendError, TryRecvError}, Receiver, Sender},
};
use tokio_native_tls::{
    native_tls::{Identity, TlsAcceptor as InnerTlsAcceptor},
    TlsAcceptor as TlsAcceptorWrapper, TlsStream,
};

type TlsAcceptResult = Result<TlsStream<TcpStream>, tokio_native_tls::native_tls::Error>;

async fn acceptor_loop(
    tls_acceptor: TlsAcceptorWrapper,
    mut conn_recv: Receiver<TcpStream>,
    result_sender: Sender<TlsAcceptResult>,
) {
    loop {
        let Some(stream) = conn_recv.recv().await else {
            break
        };
        if result_sender
            .send(tls_acceptor.accept(stream).await)
            .await
            .is_err()
        {
            break;
        }
    }
}

pub struct TlsAcceptor<'a> {
    incoming: AddrIncoming,
    conn_sender: Sender<TcpStream>,
    result_recv: Receiver<TlsAcceptResult>,
    acceptor_loop: BoxFuture<'a, ()>,
    pending_send: Option<TcpStream>,
    accepting: bool,
}

impl<'a> TlsAcceptor<'a> {
    pub fn new(identity: Identity, addr: &SocketAddr) -> anyhow::Result<Self> {
        let (conn_sender, conn_recv) = channel(1);
        let (result_sender, result_recv) = channel(1);
        Ok(Self {
            incoming: AddrIncoming::bind(addr)?,
            acceptor_loop: Box::pin(acceptor_loop(
                TlsAcceptorWrapper::from(InnerTlsAcceptor::new(identity)?),
                conn_recv,
                result_sender,
            )),
            conn_sender,
            result_recv,
            accepting: false,
            pending_send: None,
        })
    }
}

impl<'a> Accept for TlsAcceptor<'a> {
    type Conn = TlsStream<TcpStream>;

    type Error = Error;

    fn poll_accept(
        mut self: std::pin::Pin<&mut Self>,
        cx: &mut task::Context<'_>,
    ) -> Poll<Option<Result<Self::Conn, Self::Error>>> {
        if let Poll::Ready(_) = Pin::new(&mut self.acceptor_loop).poll(cx) {
            unreachable!()
        }

        if let Some(stream) = take(&mut self.pending_send) {
            if let Err(e) = self.conn_sender.try_send(stream) {
                let TrySendError::Full(stream) = e else {
                    unreachable!()
                };
                self.pending_send = Some(stream);
            }
        }

        if self.accepting {
            return match self.result_recv.try_recv() {
                Ok(x) => {
                    self.accepting = false;
                    Poll::Ready(Some(x.map_err(Into::into)))
                }
                Err(e) => match e {
                    TryRecvError::Empty => Poll::Pending,
                    TryRecvError::Disconnected => unreachable!(),
                }
            }
        }

        let stream = match Pin::new(&mut self.incoming).poll_accept(cx) {
            Poll::Ready(None) => return Poll::Ready(None),
            Poll::Ready(Some(Err(e))) => return Poll::Ready(Some(Err(e.into()))),
            Poll::Ready(Some(Ok(x))) => x.into_inner(),
            Poll::Pending => return Poll::Pending,
        };

        if let Err(e) = self.conn_sender.try_send(stream) {
            let TrySendError::Full(stream) = e else {
                unreachable!()
            };
            self.pending_send = Some(stream);
        }
        self.accepting = true;

        Poll::Pending
    }
}
