use core::option::Option;
use libc::{c_char, c_int, c_uint, c_void, nlattr, nlmsghdr, pid_t, socklen_t, FILE};

pub const MNL_SOCKET_AUTOPID: c_int = 0;
pub const MNL_ALIGNTO: c_int = 4;

pub const MNL_CB_ERROR: c_int = -1;
pub const MNL_CB_STOP: c_int = 0;
pub const MNL_CB_OK: c_int = 1;

pub const MNL_TYPE_UNSPEC: mnl_attr_data_type = 0;
pub const MNL_TYPE_U8: mnl_attr_data_type = 1;
pub const MNL_TYPE_U16: mnl_attr_data_type = 2;
pub const MNL_TYPE_U32: mnl_attr_data_type = 3;
pub const MNL_TYPE_U64: mnl_attr_data_type = 4;
pub const MNL_TYPE_STRING: mnl_attr_data_type = 5;
pub const MNL_TYPE_FLAG: mnl_attr_data_type = 6;
pub const MNL_TYPE_MSECS: mnl_attr_data_type = 7;
pub const MNL_TYPE_NESTED: mnl_attr_data_type = 8;
pub const MNL_TYPE_NESTED_COMPAT: mnl_attr_data_type = 9;
pub const MNL_TYPE_NUL_STRING: mnl_attr_data_type = 10;
pub const MNL_TYPE_BINARY: mnl_attr_data_type = 11;
pub const MNL_TYPE_MAX: mnl_attr_data_type = 12;

pub type mnl_attr_data_type = u32;

pub type mnl_attr_cb_t =
    Option<unsafe extern "C" fn(attr: *const nlattr, data: *mut c_void) -> c_int>;

pub type mnl_cb_t = Option<unsafe extern "C" fn(nlh: *const nlmsghdr, data: *mut c_void) -> c_int>;


#[repr(C)]
pub struct mnl_socket(c_void);

#[repr(C)]
pub struct mnl_nlmsg_batch(c_void);

extern "C" {
    pub fn mnl_socket_open(bus: c_int) -> *mut mnl_socket;

    #[cfg(feature = "mnl-1-0-4")]
    pub fn mnl_socket_open2(bus: c_int, flags: c_int) -> *mut mnl_socket;

    #[cfg(feature = "mnl-1-0-4")]
    pub fn mnl_socket_fdopen(fd: c_int) -> *mut mnl_socket;

    pub fn mnl_socket_bind(nl: *mut mnl_socket, groups: c_uint, pid: pid_t) -> c_int;

    pub fn mnl_socket_close(nl: *mut mnl_socket) -> c_int;

    pub fn mnl_socket_get_fd(nl: *const mnl_socket) -> c_int;

    pub fn mnl_socket_get_portid(nl: *const mnl_socket) -> c_uint;

    pub fn mnl_socket_sendto(nl: *const mnl_socket, req: *const c_void, siz: usize) -> isize;

    pub fn mnl_socket_recvfrom(nl: *const mnl_socket, buf: *mut c_void, siz: usize) -> isize;

    pub fn mnl_socket_setsockopt(
        nl: *const mnl_socket,
        type_: c_int,
        buf: *mut c_void,
        len: socklen_t,
    ) -> c_int;

    pub fn mnl_socket_getsockopt(
        nl: *const mnl_socket,
        type_: c_int,
        buf: *mut c_void,
        len: *mut socklen_t,
    ) -> c_int;

    pub fn mnl_nlmsg_size(len: usize) -> usize;

    pub fn mnl_nlmsg_get_payload_len(nlh: *const nlmsghdr) -> usize;

    pub fn mnl_nlmsg_put_header(buf: *mut c_void) -> *mut nlmsghdr;

    pub fn mnl_nlmsg_put_extra_header(nlh: *mut nlmsghdr, size: usize) -> *mut c_void;

    pub fn mnl_nlmsg_ok(nlh: *const nlmsghdr, len: c_int) -> bool;

    pub fn mnl_nlmsg_next(nlh: *const nlmsghdr, len: *mut c_int) -> *mut nlmsghdr;

    pub fn mnl_nlmsg_seq_ok(nlh: *const nlmsghdr, seq: c_uint) -> bool;

    pub fn mnl_nlmsg_portid_ok(nlh: *const nlmsghdr, portid: c_uint) -> bool;

    pub fn mnl_nlmsg_get_payload(nlh: *const nlmsghdr) -> *mut c_void;

    pub fn mnl_nlmsg_get_payload_offset(nlh: *const nlmsghdr, offset: usize) -> *mut c_void;

    pub fn mnl_nlmsg_get_payload_tail(nlh: *const nlmsghdr) -> *mut c_void;

    pub fn mnl_nlmsg_fprintf(
        fd: *mut FILE,
        data: *const c_void,
        datalen: usize,
        extra_header_size: usize,
    );

    pub fn mnl_nlmsg_batch_start(buf: *mut c_void, bufsiz: usize) -> *mut mnl_nlmsg_batch;

    pub fn mnl_nlmsg_batch_next(b: *mut mnl_nlmsg_batch) -> bool;

    pub fn mnl_nlmsg_batch_stop(b: *mut mnl_nlmsg_batch);

    pub fn mnl_nlmsg_batch_size(b: *mut mnl_nlmsg_batch) -> usize;

    pub fn mnl_nlmsg_batch_reset(b: *mut mnl_nlmsg_batch);

    pub fn mnl_nlmsg_batch_head(b: *mut mnl_nlmsg_batch) -> *mut c_void;

    pub fn mnl_nlmsg_batch_current(b: *mut mnl_nlmsg_batch) -> *mut c_void;

    pub fn mnl_nlmsg_batch_is_empty(b: *mut mnl_nlmsg_batch) -> bool;

    pub fn mnl_attr_get_type(attr: *const nlattr) -> u16;

    pub fn mnl_attr_get_len(attr: *const nlattr) -> u16;

    pub fn mnl_attr_get_payload_len(attr: *const nlattr) -> u16;

    pub fn mnl_attr_get_payload(attr: *const nlattr) -> *mut c_void;

    pub fn mnl_attr_get_u8(attr: *const nlattr) -> u8;

    pub fn mnl_attr_get_u16(attr: *const nlattr) -> u16;

    pub fn mnl_attr_get_u32(attr: *const nlattr) -> u32;

    pub fn mnl_attr_get_u64(attr: *const nlattr) -> u64;

    pub fn mnl_attr_get_str(attr: *const nlattr) -> *const c_char;

    pub fn mnl_attr_put(nlh: *mut nlmsghdr, type_: u16, len: usize, data: *const c_void);

    pub fn mnl_attr_put_u8(nlh: *mut nlmsghdr, type_: u16, data: u8);

    pub fn mnl_attr_put_u16(nlh: *mut nlmsghdr, type_: u16, data: u16);

    pub fn mnl_attr_put_u32(nlh: *mut nlmsghdr, type_: u16, data: u32);

    pub fn mnl_attr_put_u64(nlh: *mut nlmsghdr, type_: u16, data: u64);

    pub fn mnl_attr_put_str(nlh: *mut nlmsghdr, type_: u16, data: *const c_char);

    pub fn mnl_attr_put_strz(nlh: *mut nlmsghdr, type_: u16, data: *const c_char);

    pub fn mnl_attr_put_check(
        nlh: *mut nlmsghdr,
        buflen: usize,
        type_: u16,
        len: usize,
        data: *const c_void,
    ) -> bool;

    pub fn mnl_attr_put_u8_check(nlh: *mut nlmsghdr, buflen: usize, type_: u16, data: u8) -> bool;

    pub fn mnl_attr_put_u16_check(nlh: *mut nlmsghdr, buflen: usize, type_: u16, data: u16)
        -> bool;

    pub fn mnl_attr_put_u32_check(nlh: *mut nlmsghdr, buflen: usize, type_: u16, data: u32)
        -> bool;

    pub fn mnl_attr_put_u64_check(nlh: *mut nlmsghdr, buflen: usize, type_: u16, data: u64)
        -> bool;

    pub fn mnl_attr_put_str_check(
        nlh: *mut nlmsghdr,
        buflen: usize,
        type_: u16,
        data: *const c_char,
    ) -> bool;

    pub fn mnl_attr_put_strz_check(
        nlh: *mut nlmsghdr,
        buflen: usize,
        type_: u16,
        data: *const c_char,
    ) -> bool;

    pub fn mnl_attr_nest_start(nlh: *mut nlmsghdr, type_: u16) -> *mut nlattr;

    pub fn mnl_attr_nest_start_check(nlh: *mut nlmsghdr, buflen: usize, type_: u16) -> *mut nlattr;

    pub fn mnl_attr_nest_end(nlh: *mut nlmsghdr, start: *mut nlattr);

    pub fn mnl_attr_nest_cancel(nlh: *mut nlmsghdr, start: *mut nlattr);

    pub fn mnl_attr_type_valid(attr: *const nlattr, maxtype: u16) -> c_int;

    pub fn mnl_attr_validate(attr: *const nlattr, type_: mnl_attr_data_type) -> c_int;

    pub fn mnl_attr_validate2(attr: *const nlattr, type_: mnl_attr_data_type, len: usize) -> c_int;

    pub fn mnl_attr_ok(attr: *const nlattr, len: c_int) -> bool;

    pub fn mnl_attr_next(attr: *const nlattr) -> *mut nlattr;

    pub fn mnl_attr_parse(
        nlh: *const nlmsghdr,
        offset: c_uint,
        cb: mnl_attr_cb_t,
        data: *mut c_void,
    ) -> c_int;

    pub fn mnl_attr_parse_nested(
        attr: *const nlattr,
        cb: mnl_attr_cb_t,
        data: *mut c_void,
    ) -> c_int;

    pub fn mnl_attr_parse_payload(
        payload: *const c_void,
        payload_len: usize,
        cb: mnl_attr_cb_t,
        data: *mut c_void,
    ) -> c_int;

    pub fn mnl_cb_run(
        buf: *const c_void,
        numbytes: usize,
        seq: c_uint,
        portid: c_uint,
        cb_data: mnl_cb_t,
        data: *mut c_void,
    ) -> c_int;

    pub fn mnl_cb_run2(
        buf: *const c_void,
        numbytes: usize,
        seq: c_uint,
        portid: c_uint,
        cb_data: mnl_cb_t,
        data: *mut c_void,
        cb_ctl_array: *mut mnl_cb_t,
        cb_ctl_array_len: c_uint,
    ) -> c_int;
}
