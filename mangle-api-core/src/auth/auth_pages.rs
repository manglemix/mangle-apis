use std::{borrow::Cow, mem::transmute, sync::Arc};

use axum::extract::FromRef;

pub struct AuthPagesSrc {
    pub late: String,
    pub invalid: String,
    pub internal_error: String,
    pub success: String,
}

#[derive(Clone)]
pub struct AuthPages {
    _src: Arc<AuthPagesSrc>,
    pub(crate) late: Cow<'static, String>,
    pub(crate) invalid: Cow<'static, String>,
    pub(crate) internal_error: Cow<'static, String>,
    pub(crate) success: Cow<'static, String>,
}

impl AuthPages {
    pub fn new(src: AuthPagesSrc) -> Self {
        let _src = Arc::new(src);
        // Borrows of fields are tied to this struct, not static
        // This is fine because this struct holds an Arc of the src,
        // so borrows will be valid for as long as the struct is alive
        unsafe {
            Self {
                late: Cow::Borrowed(transmute(&_src.late)),
                invalid: Cow::Borrowed(transmute(&_src.invalid)),
                internal_error: Cow::Borrowed(transmute(&_src.internal_error)),
                success: Cow::Borrowed(transmute(&_src.success)),
                _src,
            }
        }
    }

    pub fn borrow_late(&self) -> &str {
        &self.late
    }

    pub fn borrow_invalid(&self) -> &str {
        &self.invalid
    }

    pub fn borrow_internal_error(&self) -> &str {
        &self.internal_error
    }

    pub fn borrow_success(&self) -> &str {
        &self.success
    }

    pub fn set_late(&mut self, late: String) {
        self.late = Cow::Owned(late)
    }

    pub fn set_invalid(&mut self, invalid: String) {
        self.invalid = Cow::Owned(invalid)
    }

    pub fn set_internal_error(&mut self, internal_error: String) {
        self.internal_error = Cow::Owned(internal_error)
    }

    pub fn set_success(&mut self, success: String) {
        self.success = Cow::Owned(success)
    }
}