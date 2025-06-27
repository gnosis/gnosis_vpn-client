use mnl_sys::{self, libc};

use std::{io, ptr};


/// The result of processing a batch of netlink responses.
pub enum CbResult {
    /// Everything went fine and this batch is finished processing.
    Stop,
    /// Everything went fine, but we expect more messages to come back from the kernel for this
    /// batch.
    Ok,
}

/// Callback function signature.
/// TODO: Write abstraction for `nlmsghdr` that can reach all fields and payload.
pub type Callback<T> = fn(msg: &libc::nlmsghdr, data: &mut T) -> libc::c_int;


/// Callback runqueue for netlink messages. Checks that all netlink messages in `buffer` are OK.
pub fn cb_run(buffer: &[u8], seq: u32, portid: u32) -> io::Result<CbResult> {
    let len = buffer.len();
    let buf = buffer.as_ptr() as *const libc::c_void;
    log::debug!("Processing {} byte netlink message without a callback", len);
    match unsafe { mnl_sys::mnl_cb_run(buf, len, seq, portid, None, ptr::null_mut()) } {
        i if i <= mnl_sys::MNL_CB_ERROR => Err(io::Error::last_os_error()),
        mnl_sys::MNL_CB_STOP => Ok(CbResult::Stop),
        _ => Ok(CbResult::Ok),
    }
}

/// Callback runqueue for netlink messages. Checks that all netlink messages in `buffer` are OK.
/// Calls the given `callback` if needed.
pub fn cb_run2<T>(
    buffer: &[u8],
    seq: u32,
    portid: u32,
    callback: Callback<T>,
    data: &mut T,
) -> io::Result<CbResult> {
    let len = buffer.len();
    let buf = buffer.as_ptr() as *const libc::c_void;
    let mut callback_context = CallbackContext { callback, data };
    log::debug!("Processing {} byte netlink message with callback", len);
    match unsafe {
        mnl_sys::mnl_cb_run(
            buf,
            len,
            seq,
            portid,
            Some(callback_wrapper::<T>),
            &mut callback_context as *mut _ as *mut libc::c_void,
        )
    } {
        i if i <= mnl_sys::MNL_CB_ERROR => Err(io::Error::last_os_error()),
        mnl_sys::MNL_CB_STOP => Ok(CbResult::Stop),
        _ => Ok(CbResult::Ok),
    }
}


/// Internal struct for helping to convert the unsafe FFI callback to the safe `Callback`.
struct CallbackContext<'a, T> {
    pub callback: Callback<T>,
    pub data: &'a mut T,
}

/// Internal FFI callback converting the callback from libmnl into a `Callback<T>` callback.
extern "C" fn callback_wrapper<T>(
    nlh: *const libc::nlmsghdr,
    data: *mut libc::c_void,
) -> libc::c_int {
    let context: &mut CallbackContext<'_, T> =
        unsafe { &mut *(data as *mut CallbackContext<'_, T>) };
    (context.callback)(unsafe { &*nlh }, context.data)
}
