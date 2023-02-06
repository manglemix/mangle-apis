use std::sync::{
    // atomic::{AtomicBool, Ordering},
    Arc
};

#[derive(Clone)]
pub struct AliveFlag(std::sync::mpsc::Sender<()>);
#[derive(Clone)]
pub struct AliveTracker(Arc<std::sync::mpsc::Receiver<()>>);


impl AliveTracker {
    pub fn is_alive(&self) -> bool {
        match self.0.try_recv() {
            Ok(_) => unreachable!(),
            Err(x) => match x {
                std::sync::mpsc::TryRecvError::Empty => true,
                std::sync::mpsc::TryRecvError::Disconnected => false,
            }
        }
    }
    pub fn wait_for_death(&self) {
        let _ = self.0.recv();
    }
}


pub fn aliveness_pair() -> (AliveFlag, AliveTracker) {
    let (sender, receiver) = std::sync::mpsc::channel();
    (AliveFlag(sender), AliveTracker(Arc::new(receiver)))
}


// pub struct ProtectWhileAlive<T>(T, AliveTracker);


// impl<T> ProtectWhileAlive<T> {
//     pub fn new(inner: T, tracker: AliveTracker) -> Self {
//         Self(inner, tracker)
//     }

//     pub fn try_unwrap(self) -> Result<T, Self> {
//         if self.1.is_alive() {
//             Err(self)
//         } else {
//             Ok(self.0)
//         }
//     }

//     pub fn unwrap(self) -> T {
//         if self.1.is_alive() {
//             panic!("Attempted to unwrap protected value while protector is still alive")
//         } else {
//             self.0
//         }
//     }

//     pub fn blocking_unwrap(self) -> T {
//         self.1.wait_for_death();
//         self.0
//     }
// }


// #[derive(Clone)]
// pub struct AsyncAliveFlag(tokio::sync::mpsc::UnboundedSender<()>);
// pub struct AsyncAliveTracker(tokio::sync::Mutex<tokio::sync::mpsc::UnboundedReceiver<()>>, AtomicBool);


// impl AsyncAliveTracker {
//     pub async fn wait_for_death(&self) {
//         if !self.1.load(Ordering::Acquire) {
//             let _ = self.0.lock().await.recv().await;
//             self.1.store(true, Ordering::Release);
//         }
//     }
// }


// pub fn async_aliveness_pair() -> (AsyncAliveFlag, AsyncAliveTracker) {
//     let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
//     (AsyncAliveFlag(sender), AsyncAliveTracker(tokio::sync::Mutex::new(receiver), AtomicBool::new(false)))
// }