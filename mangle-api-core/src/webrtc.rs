use derive_more::From;
use futures::{stream::FuturesUnordered, StreamExt};
use std::{
    hash::Hash,
    ops::{Deref, DerefMut},
    sync::Arc,
};
use tokio::{
    select,
    sync::{broadcast, mpsc, oneshot},
};

use dashmap::{
    mapref::{entry::Entry, one::RefMut},
    DashMap,
};

#[derive(From)]
pub struct SDPOffer(pub String);
#[derive(From)]
pub struct SDPAnswer(pub String);
#[derive(From)]
pub struct ICECandidate(pub String);

pub struct ICEReceiver(oneshot::Receiver<ICECandidate>);

impl ICEReceiver {
    pub async fn get_ice(self) -> ICECandidate {
        self.0.await.expect("ice to be received")
    }
}

pub struct SDPAnswerStreamSender {
    index: usize,
    answer_sender: oneshot::Sender<(usize, SDPAnswer, ICECandidate, ICESender)>,
}

impl SDPAnswerStreamSender {
    pub fn send_answer(self, sdp_answer: SDPAnswer, ice: ICECandidate) -> ICEReceiver {
        let (ice_sender, ice_recv) = oneshot::channel();
        self.answer_sender
            .send((self.index, sdp_answer, ice, ICESender(ice_sender)))
            .or(Err(()))
            .expect("send to work");
        ICEReceiver(ice_recv)
    }
}

pub struct SDPOfferStream {
    pub sdp_offer: SDPOffer,
    pub answer_stream: SDPAnswerStreamSender,
}

pub struct ConnectionReceiver {
    conn_stream_recv: mpsc::Receiver<SDPOfferStream>,
    alive_recv: broadcast::Receiver<()>,
}

impl ConnectionReceiver {
    pub async fn wait_for_conn(&mut self) -> Option<SDPOfferStream> {
        select! {
            conn = self.conn_stream_recv.recv() => {
                Some(conn.expect("recv to work"))
            }
            _ = self.alive_recv.recv() => {
                None
            }
        }
    }
}

pub struct HostConnectionReceiver<'a, K: Hash + Eq + Clone> {
    conn_recv: ConnectionReceiver,
    id: K,
    manager: &'a WebRTCSessionManager<K>,
}

impl<'a, K: Hash + Eq + Clone> Deref for HostConnectionReceiver<'a, K> {
    type Target = ConnectionReceiver;

    fn deref(&self) -> &Self::Target {
        &self.conn_recv
    }
}

impl<'a, K: Hash + Eq + Clone> DerefMut for HostConnectionReceiver<'a, K> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn_recv
    }
}

impl<'a, K: Hash + Eq + Clone> Drop for HostConnectionReceiver<'a, K> {
    fn drop(&mut self) {
        self.manager.inner.sessions.remove(&self.id);
    }
}

pub struct ICESender(oneshot::Sender<ICECandidate>);

impl ICESender {
    pub async fn send(self, ice: ICECandidate) {
        self.0.send(ice).or(Err(())).expect("ice to be sent");
    }
}

pub struct SDPAnswerStreamReceivers(
    FuturesUnordered<oneshot::Receiver<(usize, SDPAnswer, ICECandidate, ICESender)>>,
);

impl SDPAnswerStreamReceivers {
    pub async fn wait_for_an_answer(
        &mut self,
    ) -> Option<(usize, SDPAnswer, ICECandidate, ICESender)> {
        self.0.next().await.map(|x| x.expect("recv to work"))
    }
}

pub struct SDPOfferStreamSender<'a, K> {
    ref_mut: RefMut<'a, K, WebRTCSession>,
    member_count: usize,
    max_size: usize,
}

impl<'a, K> SDPOfferStreamSender<'a, K>
where
    K: Hash + Eq,
{
    pub fn get_member_count(&self) -> usize {
        self.member_count
    }

    pub async fn send_sdp_offers(
        mut self,
        offers: Vec<SDPOffer>,
    ) -> Result<
        (ConnectionReceiver, SDPAnswerStreamReceivers),
        (SDPOfferStreamSender<'a, K>, Vec<SDPOffer>),
    > {
        if offers.len() != self.member_count {
            return Err((self, offers));
        }

        let answer_receivers = FuturesUnordered::new();

        for (fut, stream_sender) in offers
            .into_iter()
            .zip(self.ref_mut.peers.iter())
            .enumerate()
            .map(|(index, (sdp_offer, offer_sender))| {
                let (answer_sender, answer_recv) = oneshot::channel();
                let answer_stream = SDPAnswerStreamSender {
                    index,
                    answer_sender,
                };

                (
                    offer_sender.send(SDPOfferStream {
                        sdp_offer,
                        answer_stream,
                    }),
                    answer_recv,
                )
            })
        {
            fut.await.or(Err(())).expect("send to work");
            answer_receivers.push(stream_sender);
        }

        let (offer_sender, conn_stream_recv) = mpsc::channel(self.max_size);
        self.ref_mut.peers.push(offer_sender);
        let alive_recv = self.ref_mut.alive_sender.subscribe();

        Ok((
            ConnectionReceiver {
                conn_stream_recv,
                alive_recv,
            },
            SDPAnswerStreamReceivers(answer_receivers),
        ))
    }
}

pub struct WebRTCSession {
    peers: Vec<mpsc::Sender<SDPOfferStream>>,
    max_size: usize,
    alive_sender: broadcast::Sender<()>,
}

pub trait RandomID: Sized {
    fn generate() -> Self;
}

struct WebRTCSessionManagerInner<K>
where
    K: Hash + Eq + Clone,
{
    sessions: DashMap<K, WebRTCSession>,
}

pub enum JoinSessionError {
    NotFound,
    Full,
}


pub struct ExistingSessionError;


#[derive(Clone)]
pub struct WebRTCSessionManager<K>
where
    K: Hash + Eq + Clone,
{
    inner: Arc<WebRTCSessionManagerInner<K>>,
}

impl<K> WebRTCSessionManager<K>
where
    K: Hash + Eq + Clone,
{
    pub fn host_session(&self, id: K, max_size: usize) -> Result<HostConnectionReceiver<K>, ExistingSessionError> {
        let Entry::Vacant(slot) = self.inner.sessions.entry(id.clone()) else { return Err(ExistingSessionError)};
        let (sender, conn_stream_recv) = mpsc::channel(max_size);
        let (alive_sender, alive_recv) = broadcast::channel(0);

        slot.insert(WebRTCSession {
            peers: vec![sender],
            max_size,
            alive_sender,
        });
        Ok(HostConnectionReceiver {
            manager: self,
            id,
            conn_recv: ConnectionReceiver {
                conn_stream_recv,
                alive_recv,
            },
        })
    }

    pub fn join_session(&self, id: &K) -> Result<SDPOfferStreamSender<K>, JoinSessionError> {
        {
            let session = self
                .inner
                .sessions
                .get(id)
                .ok_or(JoinSessionError::NotFound)?;
            if session.peers.len() >= session.max_size {
                return Err(JoinSessionError::Full);
            }
        }
        let ref_mut = self.inner.sessions.get_mut(id).unwrap();
        Ok(SDPOfferStreamSender {
            member_count: ref_mut.peers.len(),
            max_size: ref_mut.max_size,
            ref_mut,
        })
    }
}

impl<K> WebRTCSessionManager<K>
where
    K: Hash + Eq + Clone + RandomID,
{
    pub fn host_session_random_id(&self, max_size: usize) -> (HostConnectionReceiver<K>, K) {
        loop {
            let id = K::generate();
            let Ok(handle) = self.host_session(id.clone(), max_size) else { continue };
            break (handle, id);
        }
    }
}

impl<K> Default for WebRTCSessionManager<K>
where
    K: Hash + Eq + Clone,
{
    fn default() -> Self {
        Self {
            inner: Arc::new(WebRTCSessionManagerInner {
                sessions: DashMap::default(),
            }),
        }
    }
}
