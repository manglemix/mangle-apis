use std::{ops::{Deref, DerefMut}, mem::take};

use crate::sync::AliveFlag;

use super::{MangleDB, error::WriteError, serde::{ToBytes, FromBytes}};


pub struct MirroredStruct<T: ToBytes + FromBytes> {
    name: String,
    namespace: &'static str,
    db: MangleDB,
    alive_flag: AliveFlag,
    data: Option<T>
}


impl<T: ToBytes + FromBytes> Drop for MirroredStruct<T> {
    fn drop(&mut self) {
        self.db.drop_mirrored_data(self.namespace, &self.name)
    }
}


impl<'a, T: ToBytes + FromBytes> Deref for MirroredStruct<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        self.data.as_ref().unwrap()
    }
}


impl<'a, T: ToBytes + FromBytes> DerefMut for MirroredStruct<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.data.as_mut().unwrap()
    }
}


impl<'a, T: ToBytes + FromBytes> MirroredStruct<T> {
    pub fn write_to_db(mut self) -> Result<(), WriteError> {
        self.db.write_record(
            self.namespace,
            self.name.clone(),
            take(&mut self.data)
                .unwrap()
                .to_bytes()
        )
    }
}
