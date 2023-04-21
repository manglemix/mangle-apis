#![feature(const_type_id)]
#![feature(const_trait_impl)]
#![feature(const_transmute_copy)]
#![feature(const_refs_to_cell)]
//! Useful utility methods

use std::{
    any::{Any, TypeId},
    mem::{forget, transmute_copy},
};

// pub fn try_cast_ref<S: Any, D: Any + ?Sized>(src: &S) -> Option<&D> {
//     if src.type_id() == TypeId::of::<D>() {
//         unsafe { Some(transmute_copy(src)) }
//     } else {
//         None
//     }
// }

/// Casts one type into another without heap allocations
    ///
    /// If `S == D`, then this function will safely return `Ok(D)` else, `Err(S)`
    ///
    /// This function seems like it doesn't do anything unless `S` & `D` are equal,
    /// in which case it just returns what was passed.
    /// However, the power of this function comes when you pass a generic type
    /// in to downcast into a concrete type, or when you pass a concrete type
    /// and to upcast into a generic type!
    ///
    /// ## Example
    /// ```
    /// use manglext::AnyExt;
    /// use std::any::Any;
    ///
    /// fn generic_in<T: Any>(x: T) {
    ///     if let Ok(x) = x.try_cast::<String>() {
    ///         println!("You passed a string! {x}");    
    ///     }
    /// }
    /// fn generic_out<T: Any>(x: String) -> Option<T> {
    ///     if let Ok(x) = x.try_cast::<T>() {
    ///         println!("We are returning a string!");  
    ///         Some(x)  
    ///     } else {
    ///         println!("I don't know what we are returning");  
    ///         None
    ///     }
    /// }
    /// ```
pub const fn try_cast<S: Any, D: Any>(src: S) -> Result<D, S> {
    if TypeId::of::<S>() == TypeId::of::<D>() {
        // SAFETY: Only one copy of src will exist in the end
        // S and D are also guaranteed to be the same size, as
        // they are the same type, and only Sized types can be
        // passed to and returned from functions
        unsafe {
            let dest = transmute_copy(&src);
            forget(src);
            Ok(dest)
        }
    } else {
        Err(src)
    }
}


pub fn immut_leak<'a, T: 'a>(src: T) -> &'a T {
    mut_leak(src)
}


pub fn mut_leak<'a, T: 'a>(src: T) -> &'a mut T {
    Box::leak(Box::new(src))
}


#[cfg(test)]
mod tests {
    // use std::{mem::transmute, ops::Deref};

    // use super::*;

    // fn generic_in<T: Any>(x: T) -> Option<String> {
    //     x.try_cast().ok()
    // }

    // fn generic_out<T: Any>(x: String) -> Option<T> {
    //     x.try_cast().ok()
    // }

    // #[test]
    // fn basic() {
    //     assert!(generic_in("fail").is_none());
    //     assert!(generic_in("works".to_string()).is_some());

    //     assert!(generic_out::<String>("works".to_string()).is_some());
    // }

    // #[test]
    // fn traits() {
    //     let string = "test".to_string();
    //     let test: Box<dyn Any> = Box::new(string);
    //     let test: &dyn Any = unsafe { transmute(test.deref()) };
    //     let _out: &dyn Any = test.try_cast().unwrap();
    //     // let out: Box<dyn Display> = try_cast(Box::new(string)).unwrap();

    //     // assert!(format!("{out}") == "test");
    // }
}
