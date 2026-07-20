//! Root-gated repro/regression tests for the TUN fd handoff on macOS.
//!
//! The daemon passes a utun kernel-control-socket fd from root to the worker via
//! `SCM_RIGHTS` (`gnosis_vpn_lib::socket::fd_passing`). These tests replicate
//! that descriptor flow in isolation - utun created as `routing::tun::Tun::create`
//! does, dup'd as `routing_actor::setup_routing` does, passed across a socketpair
//! under several conditions (bare and configured interface, concurrent process
//! spawns, cross-process peers) - and pin down the macOS kernel bug behind the
//! first-connect `EINVAL`: closing a socket end while its `SCM_RIGHTS` copy is in
//! flight poisons the socketpair for all later rights transfers, which is why
//! `setup_worker` retains the worker's end in `WorkerChild`.
//!
//! Creating utun devices needs root, so the tests are `#[ignore]`d and skip
//! themselves when not run as root:
//!
//! ```sh
//! sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored
//! ```

#![cfg(target_os = "macos")]

use std::os::fd::{AsRawFd, BorrowedFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use gnosis_vpn_lib::socket::fd_passing;
use neptun::device::tun::TunSocket;

const ITERATIONS: usize = 200;

fn is_root() -> bool {
    // SAFETY: geteuid has no preconditions and cannot fail.
    let euid = unsafe { libc::geteuid() };
    if euid != 0 {
        eprintln!("skipping: creating utun devices requires root (re-run under sudo)");
        return false;
    }
    true
}

/// Create a utun device the way `routing::tun::Tun::create` does: dup the device
/// socket cloexec and drop the original `TunSocket`, so the dup alone keeps the
/// interface alive. Returns the device fd and the kernel-assigned interface name.
fn create_utun() -> (OwnedFd, String) {
    let socket = TunSocket::new("utun").expect("create utun device");
    let name = socket.name().expect("resolve utun name");
    // SAFETY: `socket` owns the descriptor and stays alive until `drop(socket)`
    // below, so the borrow cannot outlive an open fd.
    let borrowed = unsafe { BorrowedFd::borrow_raw(socket.as_raw_fd()) };
    let device_fd = rustix::io::fcntl_dupfd_cloexec(borrowed, 0).expect("dup utun fd");
    drop(socket);
    (device_fd, name)
}

/// Dup the device fd once more, as `routing_actor::setup_routing` does before the
/// fd crosses the actor channel to be sent to the worker.
fn worker_fd(device_fd: &OwnedFd) -> OwnedFd {
    rustix::io::fcntl_dupfd_cloexec(device_fd, 0).expect("dup utun fd for handoff")
}

fn utun_worker_fd() -> OwnedFd {
    let (device_fd, _name) = create_utun();
    worker_fd(&device_fd)
}

/// Configure the interface exactly like `routing::macos::configure_interface`
/// does before the daemon passes the fd: assign a point-to-point address, bring
/// the interface up, set the MTU.
fn configure_utun(name: &str, address: &str, mtu: u32) {
    let status = std::process::Command::new("ifconfig")
        .args([name, "inet", address, address, "up"])
        .status()
        .expect("run ifconfig addr/up");
    assert!(status.success(), "ifconfig {name} inet {address} {address} up failed");
    let status = std::process::Command::new("ifconfig")
        .args([name, "mtu", &mtu.to_string()])
        .status()
        .expect("run ifconfig mtu");
    assert!(status.success(), "ifconfig {name} mtu {mtu} failed");
}

fn pass_fd_once(fd: &OwnedFd, context: &str) {
    let (a, b) = UnixStream::pair().expect("socketpair");
    fd_passing::send_fd(&a, fd)
        .unwrap_or_else(|error| panic!("send_fd failed ({context}): {error}; {}", fd_passing::diagnose(&a, fd)));
    let received = fd_passing::recv_fd(&b).expect("recv_fd");
    let stat = rustix::fs::fstat(&received).expect("fstat received fd");
    let file_type = rustix::fs::FileType::from_raw_mode(stat.st_mode as _);
    assert_eq!(
        file_type,
        rustix::fs::FileType::Socket,
        "{context}: received fd is not a socket"
    );
}

fn pass_utun_fd_once(iteration: usize) {
    let fd = utun_worker_fd();
    pass_fd_once(&fd, &format!("bare utun, iteration {iteration}"));
}

#[test]
#[ignore = "requires root: sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored"]
fn utun_fd_passes_over_scm_rights_repeatedly() {
    if !is_root() {
        return;
    }
    for iteration in 0..ITERATIONS {
        pass_utun_fd_once(iteration);
    }
}

#[test]
#[ignore = "requires root: sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored"]
fn configured_utun_fd_passes_over_scm_rights() {
    if !is_root() {
        return;
    }
    // The daemon sends the fd only after the interface is configured and up
    // (`routing::macos::StaticRouter::setup`); a bare utun fd passes while the
    // daemon's fails, so replicate the interface state at the real send site.
    for iteration in 0..20 {
        let (device_fd, name) = create_utun();
        configure_utun(&name, "10.213.99.1", 1420);
        let fd = worker_fd(&device_fd);
        pass_fd_once(&fd, &format!("configured utun {name}, iteration {iteration}"));
    }
}

#[test]
#[ignore = "requires root: sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored"]
fn closing_an_end_while_its_scm_rights_copy_is_in_flight_poisons_the_pair() {
    // Documents the macOS kernel bug that `setup_worker` must avoid: closing a
    // socket end while its SCM_RIGHTS copy is still in flight corrupts the
    // kernel's accounting for the socketpair, after which sendmsg with rights
    // over the retained peer end fails with EINVAL - for any payload, and even
    // after the in-flight copy has been received. This is why WorkerChild
    // retains `_child_tun_socket` for the worker's lifetime.
    //
    // If this test ever fails, the kernel bug is fixed on the running macOS and
    // that retention can be reduced to a drop right after the bootstrap send.
    let mut poisoned = 0u32;
    for _ in 0..100 {
        let (parent, child) = UnixStream::pair().expect("fd-passing socketpair");
        let (boot_a, boot_b) = UnixStream::pair().expect("bootstrap socketpair");
        fd_passing::send_fd(&boot_a, &child).expect("bootstrap the child end");
        drop(child); // closed while the SCM_RIGHTS copy is still in flight
        let child_copy = UnixStream::from(fd_passing::recv_fd(&boot_b).expect("receive the child end"));

        let (pipe_r, _pipe_w) = rustix::pipe::pipe().expect("pipe");
        match fd_passing::send_fd(&parent, &pipe_r) {
            Ok(()) => {
                let _ = fd_passing::recv_fd(&child_copy).expect("recv pipe fd");
            }
            Err(error) => {
                assert_eq!(
                    error.raw_os_error(),
                    Some(libc::EINVAL),
                    "poisoned pair failed with an unexpected errno: {error}"
                );
                poisoned += 1;
            }
        }
    }
    assert!(
        poisoned > 0,
        "no send over a close-while-in-flight pair failed; the kernel bug appears fixed"
    );
}

#[test]
#[ignore = "requires root: sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored"]
fn utun_fd_passes_when_the_bootstrapped_end_stays_open() {
    if !is_root() {
        return;
    }
    // Replicates the production lifetime (`setup_worker` retains the worker's
    // end in WorkerChild): the locally-sent end stays open while its SCM_RIGHTS
    // copy is in flight, so the pair never gets poisoned and the utun fd handoff
    // succeeds. Regression guard for the first-connect EINVAL.
    for iteration in 0..200 {
        let (parent, child) = UnixStream::pair().expect("fd-passing socketpair");
        let (boot_a, boot_b) = UnixStream::pair().expect("bootstrap socketpair");
        fd_passing::send_fd(&boot_a, &child).expect("bootstrap the child end");
        let child_copy = UnixStream::from(fd_passing::recv_fd(&boot_b).expect("receive the child end"));

        let fd = utun_worker_fd();
        fd_passing::send_fd(&parent, &fd).unwrap_or_else(|error| {
            panic!(
                "send_fd failed (kept-open end, iteration {iteration}): {error}; {}",
                fd_passing::diagnose(&parent, &fd)
            )
        });
        let received = fd_passing::recv_fd(&child_copy).expect("recv utun fd");
        let stat = rustix::fs::fstat(&received).expect("fstat received fd");
        assert_eq!(
            rustix::fs::FileType::from_raw_mode(stat.st_mode as _),
            rustix::fs::FileType::Socket,
            "kept-open end, iteration {iteration}"
        );
        drop(child);
    }
}

/// Spawn a child process that merely holds `peer` open as its stdin (as the
/// worker holds its socketpair ends), optionally dropped to an unprivileged uid.
fn spawn_holder(peer: UnixStream, uid: Option<u32>) -> std::process::Child {
    use std::os::unix::process::CommandExt;
    let mut command = std::process::Command::new("/bin/sleep");
    command.arg("30").stdin(std::process::Stdio::from(OwnedFd::from(peer)));
    if let Some(uid) = uid {
        command.uid(uid);
    }
    command.spawn().expect("spawn fd-holder child")
}

fn nobody_uid() -> u32 {
    // SAFETY: getpwnam with a valid nul-terminated name; the result is only read
    // before any other libc call that could reuse its static buffer.
    let pw = unsafe { libc::getpwnam(c"nobody".as_ptr()) };
    assert!(!pw.is_null(), "user 'nobody' must exist");
    unsafe { (*pw).pw_uid }
}

#[test]
#[ignore = "requires root: sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored"]
fn utun_fd_sends_while_an_unprivileged_process_holds_the_peer() {
    if !is_root() {
        return;
    }
    // In the daemon the peer end lives in the unprivileged worker process. The
    // send happens before the worker reads (it waits for TunnelReady), so only
    // the send needs to succeed here; the in-flight fd is released with the
    // child. Exercise both a root-owned and a uid-dropped holder.
    for (label, uid) in [("root holder", None), ("nobody holder", Some(nobody_uid()))] {
        let (parent, child) = UnixStream::pair().expect("fd-passing socketpair");
        let mut holder = spawn_holder(child, uid);

        let fd = utun_worker_fd();
        let send_result = fd_passing::send_fd(&parent, &fd);
        let report = fd_passing::diagnose(&parent, &fd);
        holder.kill().expect("kill holder");
        holder.wait().expect("reap holder");
        send_result.unwrap_or_else(|error| panic!("send_fd failed ({label}): {error}; {report}"));
    }
}

#[test]
#[ignore = "requires root: sudo -E cargo test -p gnosis_vpn-root --test tun_fd_pass -- --ignored"]
fn utun_fd_passes_while_processes_spawn_concurrently() {
    if !is_root() {
        return;
    }
    let stop = Arc::new(AtomicBool::new(false));
    let churn_stop = Arc::clone(&stop);
    let churn = std::thread::spawn(move || {
        while !churn_stop.load(Ordering::Relaxed) {
            std::process::Command::new("/usr/bin/true")
                .status()
                .expect("spawn /usr/bin/true");
        }
    });
    for iteration in 0..ITERATIONS {
        pass_utun_fd_once(iteration);
    }
    stop.store(true, Ordering::Relaxed);
    churn.join().expect("churn thread");
}
