#[macro_use]
extern crate ioctl_sys;

ioctl!(bad kiocsound with 0x4B2F);
ioctl!(none drm_ioctl_set_master with b'd', 0x1e);
ioctl!(read ev_get_version with b'E', 0x01; u32);
ioctl!(write ev_set_repeat with b'E', 0x03; [u32; 2]);

fn main() {
    let mut x = 0;
    let ret = unsafe { ev_get_version(0, &mut x) };
    println!("returned {}, x = {}", ret, x);
}
