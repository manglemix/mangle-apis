use std::{ops::{Deref, DerefMut}, collections::{VecDeque, HashMap}, sync::Arc, marker::PhantomData, mem::take};

use parking_lot::Mutex;

use crate::{serde::{FromBytes, ToBytes}, error::{WriteError, ReadError}, sync::{AliveFlag, aliveness_pair}, db::MangleDB};


pub struct UniquelyOwnedRecord<T: FromBytes + ToBytes> {
    namespace: UniquelyOwnedRecordNamespace<T>,
    name: String,
    record: Option<T>
}


impl<T: FromBytes + ToBytes> UniquelyOwnedRecord<T> {
    pub fn finalize(mut self) -> Result<(), WriteError> {
        let mut data = take(&mut self.record).unwrap().to_bytes();

        self.namespace.db.write_record(
            self.namespace.namespace,
            self.name.clone(),
            data.clone()
        )?;

        let mut write_data = "OwnedWrite".as_bytes().to_vec();
        write_data.push(0);
        write_data.append(&mut data);

        self.namespace
            .db
            .conns
            .propagate_message(
                self.namespace.namespace,
                self.name.clone(),
                write_data
            );
        
        Ok(())
    }
}


impl<T: FromBytes + ToBytes> Deref for UniquelyOwnedRecord<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.record.as_ref().unwrap()
    }
}


impl<T: FromBytes + ToBytes> DerefMut for UniquelyOwnedRecord<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.record.as_mut().unwrap()
    }
}


impl<T: FromBytes + ToBytes> Drop for UniquelyOwnedRecord<T> {
    fn drop(&mut self) {
        let mut lock = self.namespace.owned_names.lock();
        let entry = lock.get_mut(&self.name).expect("queue to exist");
        
        if entry.waitlist.pop_front().is_none() {
            entry.is_locked = false;
        }
    }
}


struct OwnedNameEntry {
    is_locked: bool,
    waitlist: VecDeque<AliveFlag>
}


pub struct UniquelyOwnedRecordNamespace<T: FromBytes + ToBytes> {
    namespace: &'static str,
    owned_names: Arc<Mutex<HashMap<String, OwnedNameEntry>>>,
    db: MangleDB,
    _phantom: PhantomData<T>
}


impl<T: FromBytes + ToBytes> Clone for UniquelyOwnedRecordNamespace<T> {
    fn clone(&self) -> Self {
        Self {
            namespace: self.namespace,
            owned_names: self.owned_names.clone(),
            db: self.db.clone(),
            _phantom: Default::default()
        }
    }
}


impl<T: FromBytes + ToBytes> UniquelyOwnedRecordNamespace<T> {
    pub fn get_record(&self, name: &String) -> Result<UniquelyOwnedRecord<T>, ReadError> {
        let mut lock = self.owned_names.lock();

        if let Some(entry) = lock.get_mut(name) {
            if entry.is_locked {
                let (flag, tracker) = aliveness_pair();
                entry.waitlist.push_back(flag);
                drop(lock);
                tracker.wait_for_death();
                lock = self.owned_names.lock();
                debug_assert!(lock.get(name).expect("name to exist").is_locked)
            }
        } else {
            drop(lock);
            todo!("get ownership")
        }

        self.db
            .read_record(self.namespace, name)
            .map(|record| UniquelyOwnedRecord {
                namespace: self.clone(),
                name: name.clone(),
                record: Some(record)
            })
    }
}