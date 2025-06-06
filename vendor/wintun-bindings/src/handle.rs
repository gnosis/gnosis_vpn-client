use windows_sys::Win32::{
    Foundation::{CloseHandle, FALSE, HANDLE},
    System::Threading::{CreateEventW, SetEvent},
};

use crate::{util::get_last_error, Error};

/// A wrapper struct that allows a type to be Send and Sync
#[derive(Copy, Clone, Debug)]
pub struct UnsafeHandle<T>(pub T);

/// We never read from the pointer. It only serves as a handle we pass to the kernel or C code that
/// doesn't have the same mutable aliasing restrictions we have in Rust
unsafe impl<T> Send for UnsafeHandle<T> {}
unsafe impl<T> Sync for UnsafeHandle<T> {}

#[derive(Debug)]
pub(crate) struct SafeEvent(pub UnsafeHandle<HANDLE>);

impl From<UnsafeHandle<HANDLE>> for SafeEvent {
    fn from(handle: UnsafeHandle<HANDLE>) -> Self {
        Self(handle)
    }
}

impl SafeEvent {
    pub(crate) fn new(manual_reset: bool, initial_state: bool) -> Result<Self, Error> {
        let null = std::ptr::null();
        let handle = unsafe { CreateEventW(null, manual_reset as _, initial_state as _, std::ptr::null()) };
        if handle.is_null() {
            return Err(get_last_error()?.into());
        }
        Ok(Self(UnsafeHandle(handle)))
    }

    pub(crate) fn set_event(&self) -> Result<(), Error> {
        if unsafe { SetEvent(self.0 .0) } == FALSE {
            return Err(get_last_error()?.into());
        }
        Ok(())
    }

    pub(crate) fn close_handle(&self) -> Result<(), Error> {
        if !self.0 .0.is_null() && unsafe { CloseHandle(self.0 .0) } == FALSE {
            return Err(get_last_error()?.into());
        }
        Ok(())
    }

    #[allow(dead_code)]
    pub(crate) fn get_handle(&self) -> UnsafeHandle<HANDLE> {
        self.0
    }
}

impl Drop for SafeEvent {
    fn drop(&mut self) {
        if let Err(e) = self.close_handle() {
            log::trace!("Failed to close event handle: {}", e);
        }
    }
}
