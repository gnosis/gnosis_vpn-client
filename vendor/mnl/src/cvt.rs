use std::io;


pub trait IsMinusOne {
    fn is_minus_one(&self) -> bool;
}

macro_rules! impl_is_minus_one {
    ($($t:ident)*) => ($(impl IsMinusOne for $t {
        fn is_minus_one(&self) -> bool {
            *self == -1
        }
    })*)
}

impl_is_minus_one! { i8 i16 i32 i64 isize }

pub trait IsError {
    fn is_error(&self) -> bool;
}

impl<T: IsMinusOne> IsError for T {
    fn is_error(&self) -> bool {
        self.is_minus_one()
    }
}

impl<T> IsError for *const T {
    fn is_error(&self) -> bool {
        (*self as *const T).is_null()
    }
}

impl<T> IsError for *mut T {
    fn is_error(&self) -> bool {
        (*self as *mut T).is_null()
    }
}

pub fn cvt<T: IsError>(t: T) -> io::Result<T> {
    if t.is_error() {
        Err(io::Error::last_os_error())
    } else {
        Ok(t)
    }
}
