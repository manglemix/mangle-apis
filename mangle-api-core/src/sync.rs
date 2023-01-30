use std::{sync::{mpsc::{Sender, Receiver, channel}, atomic::{AtomicBool, Ordering}}};

#[derive(Clone)]
pub struct AliveFlag(Sender<()>);
pub struct AliveTracker(Receiver<()>, AtomicBool);


impl AliveTracker {
    pub fn wait_for_death(&self) {
        if !self.1.load(Ordering::Acquire) {
            let _ = self.0.recv();
            self.1.store(true, Ordering::Release);
        }
    }
}


pub fn aliveness_pair() -> (AliveFlag, AliveTracker) {
    let (sender, receiver) = channel();
    (AliveFlag(sender), AliveTracker(receiver, AtomicBool::new(false)))
}