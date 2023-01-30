use std::{collections::{HashSet, HashMap}, sync::{Arc}, path::Path, borrow::Borrow, mem::transmute};

use parking_lot::{RwLock, RwLockUpgradableReadGuard};
use persy::{Persy, ByteVec};

pub mod error;
pub mod conn;
pub mod structs;
pub mod serde;

use error::{WriteError, ReadError, OwnError};

use crate::sync::{aliveness_pair, AliveTracker};

use self::{conn::SiblingConnections, structs::{MirroredStruct}, serde::{ToBytes, FromBytes}};


struct NamespaceEntry {
    namespace: String,
    owned_names: RwLock<HashSet<String>>,
    mirrored_names: RwLock<HashMap<String, AliveTracker>>
}


struct MangleDBImpl {
    namespaces: Vec<NamespaceEntry>
}


#[derive(Clone)]
pub struct MangleDB {
    persy: Persy,
    inner: Arc<MangleDBImpl>,
    conns: SiblingConnections,
}


impl MangleDB {
    pub fn open(path: impl AsRef<Path>, namespaces: Vec<String>, conns: SiblingConnections) -> anyhow::Result<Self> {
        let persy = Persy::open(path, Default::default())?;
        let mut namespaces_vec = Vec::new();

        for namespace in namespaces {
            let mut set = HashSet::new();
            for (_, content) in persy.scan(format!("_{}_owned", namespace))? {
                set.insert(String::from_utf8(content)?);
            }
            
            namespaces_vec.push(NamespaceEntry {
                namespace,
                owned_names: RwLock::new(set),
                mirrored_names: Default::default()
            });
        }

        Ok(MangleDB {
            persy,
            inner: Arc::new(MangleDBImpl {
                namespaces: namespaces_vec
            }),
            conns
        })
    }

    pub fn checked_write_record(&self, namespace: impl Into<String>, name: impl Into<String>, data: Vec<u8>) -> Result<(), WriteError> {
        let namespace = namespace.into();
        let name = name.into();
        {
            let namespace_entry = self.inner
                .namespaces
                .iter()
                .find(|x| x.namespace == namespace)
                .ok_or(WriteError::UnrecognizedNamespace)?;
            
            if let Some(x) = namespace_entry
                .mirrored_names
                .read()
                .get(&name)
            {
                x.wait_for_death();
            }

            if namespace_entry
                .owned_names
                .read()
                .contains(&name)
            {
                self.write_record(&namespace, name.clone(), data.clone())?;

                self.conns.propagate_owned_write(namespace, name, data);

                return Ok(())
            }
        }

        todo!("request ownership")
    }

    pub fn read_record<T: FromBytes>(&self, namespace: impl AsRef<str>, name: impl Borrow<String>) -> Result<T, ReadError> {
        let value = self.persy
            .one::<_, ByteVec>(namespace.as_ref(), name.borrow())?
            .expect("there to be a value");
        
        T::from_slice(&value).map_err(Into::into)
    }

    pub fn try_get_mirrored_data<T: ToBytes + FromBytes>(&self, namespace: impl AsRef<str>, name: impl Into<String>) -> Result<MirroredStruct<T>, ReadError> {
        let mut namespace = namespace.as_ref();
        let name = name.into();

        let lock = self.inner
            .namespaces
            .iter()
            .find(|x| {
                if x.namespace == namespace {
                    namespace = &x.namespace;
                    true
                } else {
                    false
                }
            })
            .map(|x| x.mirrored_names.upgradable_read())
            .ok_or(ReadError::UnrecognizedNamespace)?;
        
        if let Some(x) = lock.get(&name) {
            x.wait_for_death();
        }

        let (alive_flag, alive_tracker) = aliveness_pair();
        let mut lock = RwLockUpgradableReadGuard::upgrade(lock);
        lock.insert(name.clone(), alive_tracker);
        drop(lock);

        Ok(MirroredStruct {
            namespace: unsafe { transmute(namespace) },
            db: self.clone(),
            alive_flag,
            data: Some(self.read_record(namespace, &name)?),
            name
        })
    }

    fn drop_mirrored_data(&self, namespace: &str, name: &str) {
        self.inner
            .namespaces
            .iter()
            .find(|x| x.namespace == namespace)
            .map(|x| {
                x.mirrored_names
                    .write()
                    .remove(name);
            });
    }

    fn write_record(&self, namespace: &str, name: String, data: Vec<u8>) -> Result<(), WriteError> {
        let mut tx = self.persy.begin()?;
        tx.put(namespace, name, ByteVec::new(data))
            .map_err(WriteError::insert_error)?;
        tx.commit().map_err(Into::into)
    }

    fn own_record(&self, namespace: impl AsRef<str>, name: impl Into<String>) -> Result<(), OwnError> {
        let namespace = namespace.as_ref();
        let name = name.into();

        if self.persy.one::<_, ByteVec>(namespace, &name)?.is_none() {
            return Err(OwnError::UnrecognizedName)
        }

        self.inner
            .namespaces
            .iter()
            .find_map(move |x| {
                if x.namespace == namespace {
                    x.owned_names.write().insert(name);
                    Some(())
                } else {
                    None
                }
            })
            .ok_or(OwnError::UnrecognizedNamespace)
    }

    fn is_record_owned(&self, namespace: impl AsRef<str>, name: impl AsRef<str>) -> bool {
        let namespace = namespace.as_ref();
        let name = name.as_ref();

        self.inner
            .namespaces
            .iter()
            .find_map(|x| {
                if x.namespace == namespace {
                    Some(x.owned_names.read().contains(name))
                } else {
                    None
                }
            })
            .unwrap_or_default()
    }
}