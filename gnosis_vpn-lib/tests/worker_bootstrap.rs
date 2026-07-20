use std::env;
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::process::{Command, ExitCode, Stdio};
use std::time::Duration;

use gnosis_vpn_lib::socket::{fd_passing, worker};

const CHILD_ENV: &str = "GNOSIS_VPN_WORKER_BOOTSTRAP_TEST_CHILD";
const JSON_LINE: &str = "{\"kind\":\"startup\"}\n";
const FD_PAYLOAD: &str = "bootstrap fd payload";

fn child_main() -> io::Result<()> {
    let control = worker::claim_stdin_socket()?;
    let tun_socket = UnixStream::from(fd_passing::recv_fd(&control)?);
    worker::set_tun_fd_socket(tun_socket);

    let mut stdin_byte = [0u8; 1];
    if io::stdin().read(&mut stdin_byte)? != 0 {
        return Err(io::Error::other("stdin was not repointed to /dev/null"));
    }

    let mut line = String::new();
    BufReader::new(&control).read_line(&mut line)?;
    if line != JSON_LINE {
        return Err(io::Error::other(format!("unexpected control line: {line:?}")));
    }

    let mut received = File::from(worker::recv_tun_fd()?);
    let mut payload = vec![0u8; FD_PAYLOAD.len()];
    received.read_exact(&mut payload)?;
    if payload != FD_PAYLOAD.as_bytes() {
        return Err(io::Error::other(format!("unexpected fd payload: {payload:?}")));
    }

    (&control).write_all(b"ready\n")?;
    Ok(())
}

fn parent_main() -> io::Result<()> {
    let (root_control, worker_control) = UnixStream::pair()?;
    let (root_tun, worker_tun) = UnixStream::pair()?;
    let (pipe_reader, pipe_writer) = rustix::pipe::pipe().map_err(io::Error::from)?;
    for fd in [&pipe_reader, &pipe_writer] {
        let flags = rustix::io::fcntl_getfd(fd).map_err(io::Error::from)?;
        rustix::io::fcntl_setfd(fd, flags | rustix::io::FdFlags::CLOEXEC).map_err(io::Error::from)?;
    }
    let read_timeout = Some(Duration::from_secs(5));
    root_control.set_read_timeout(read_timeout)?;
    worker_control.set_read_timeout(read_timeout)?;
    worker_tun.set_read_timeout(read_timeout)?;

    fd_passing::send_fd(&root_control, &worker_tun)?;
    fd_passing::send_fd(&root_tun, &pipe_reader)?;
    drop(worker_tun);
    drop(pipe_reader);

    let mut child = Command::new(env::current_exe()?)
        .env(CHILD_ENV, "1")
        .stdin(Stdio::from(OwnedFd::from(worker_control)))
        .spawn()?;

    (&root_control).write_all(JSON_LINE.as_bytes())?;
    let mut pipe_writer = File::from(pipe_writer);
    pipe_writer.write_all(FD_PAYLOAD.as_bytes())?;
    drop(pipe_writer);

    let mut response = String::new();
    BufReader::new(&root_control).read_line(&mut response)?;
    if response != "ready\n" {
        return Err(io::Error::other(format!("unexpected child response: {response:?}")));
    }

    let status = child.wait()?;
    if !status.success() {
        return Err(io::Error::other(format!("bootstrap child exited with {status}")));
    }
    Ok(())
}

fn main() -> ExitCode {
    let result = if env::var_os(CHILD_ENV).is_some() {
        child_main()
    } else {
        parent_main()
    };

    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("worker bootstrap integration test failed: {error}");
            ExitCode::FAILURE
        }
    }
}
