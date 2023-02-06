use std::{collections::{HashMap, VecDeque}, path::Path, borrow::Borrow, marker::PhantomData, sync::Arc, mem::{take, MaybeUninit}, ops::{Deref, DerefMut}, cell::RefCell, rc::Rc};

use parking_lot::Mutex;
use persy::{Persy, ByteVec, ValueMode, PersyId};

use crate::{error::{WriteError, ReadError}, sync::{AliveFlag, aliveness_pair}};


use crate::{conn::{SiblingConnections}, serde::{ToBytes, FromBytes}};



pub struct UniquelyOwnedRecord<T: FromBytes + ToBytes> {
    namespace: UniquelyOwnedRecordNamespace<T>,
    name: String,
    record: Option<T>
}


impl<T: FromBytes + ToBytes> UniquelyOwnedRecord<T> {
    pub fn finalize(mut self) -> Result<(), WriteError> {
        self.namespace.db.write_record(
            self.namespace.namespace,
            self.name.clone(),
            take(&mut self.record).unwrap().to_bytes()
        )
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
    _persy_id: PersyId,
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


pub struct UninitializedNamespace<const N: usize> {
    namespaces: Rc<RefCell<[&'static str; N]>>,
    index: usize,
    db: MangleDB
    // initialized: bool
}


impl<const N: usize> UninitializedNamespace<N> {
    fn new(namespaces: Rc<RefCell<[&'static str; N]>>, index: usize, db: MangleDB) -> Self {
        Self {
            namespaces,
            index,
            db
        }
    }

    #[must_use]
    pub fn into_owned_namespace<T: FromBytes + ToBytes>(self, namespace: &'static str) -> anyhow::Result<UniquelyOwnedRecordNamespace<T>> {
        assert!(!namespace.starts_with('_'), "Namespaces must not start with underscores");
        assert!(!self.namespaces.deref().borrow().contains(&namespace), "Namespace: {namespace} already taken");

        self.namespaces.borrow_mut()[self.index] = namespace;

        if !self.db.persy.exists_index(namespace)? {
            let mut tx = self.db
                .persy
                .begin()?;
        
            tx.create_index::<String, ByteVec>(namespace, ValueMode::Replace)?;
            tx.commit()?;
        }

        let namespace_keys = format!("_{namespace}_keys");
        if !self.db.persy.exists_segment(&namespace_keys)? {
            let mut tx = self.db
                .persy
                .begin()?;
            
            tx.create_segment(&namespace_keys)?;
            tx.commit()?;
        }

        let owned_names = Default::default();

        // let owned_names_key = format!("_{namespace}_owned_keys");
        // if self.db.persy.exists_segment(&namespace_keys)? {
            
        // }

        Ok(
            UniquelyOwnedRecordNamespace {
                namespace,
                owned_names,
                db: self.db,
                _phantom: Default::default()
            }
        )
    }
}


pub struct MangleDBBuilder {
    db: MangleDB,
}


impl MangleDBBuilder {
    pub fn open(path: impl AsRef<Path>, conns: SiblingConnections) -> anyhow::Result<Self> {
        let persy = match Persy::open(&path, Default::default()) {
            Ok(x) => x,
            Err(e) => match e.error() {
                persy::OpenError::NotExists => {
                    Persy::create(&path)?;
                    Persy::open(path, Default::default())?
                }
                e => return Err(e.into())
            }
        };

        Ok(Self {
            db: MangleDB {
                persy,
                conns
            },
        })
    }

    pub fn create_namespaces<const N: usize>(self) -> [UninitializedNamespace<N>; N] {
        let mut out = MaybeUninit::uninit_array();
        let namespaces = Rc::new(RefCell::new([""; N]));

        for i in 0..N {
            out[i].write(UninitializedNamespace::new(namespaces.clone(), i, self.db.clone()));
        }

        unsafe { MaybeUninit::array_assume_init(out) }
    }
}


#[derive(Clone)]
pub(crate) struct MangleDB {
    persy: Persy,
    pub(crate) conns: SiblingConnections,
}


impl MangleDB {
    pub(crate) fn read_record<T: FromBytes>(&self, namespace: impl AsRef<str>, name: impl Borrow<String>) -> Result<T, ReadError> {
        let value = self.persy
            .one::<_, ByteVec>(namespace.as_ref(), name.borrow())?
            .expect("there to be a value");
        
        T::from_bytes(value.into()).map_err(Into::into)
    }

    pub(crate) fn write_record(&self, namespace: &str, name: String, data: Vec<u8>) -> Result<(), WriteError> {
        let mut tx = self.persy.begin()?;
        tx.put(namespace, name, ByteVec::new(data))
            .map_err(WriteError::insert_error)?;
        tx.commit().map_err(Into::into)
    }
}