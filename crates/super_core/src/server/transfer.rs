//! Listening sockets handed over from a running process to the one replacing it (a graceful
//! upgrade): the new process waits on a Unix socket, and the previous one sends its listening
//! sockets there (`SCM_RIGHTS`), with the address of each, before it stops accepting

use std::{
    io::{IoSlice, IoSliceMut},
    os::{
        fd::{AsRawFd, FromRawFd, OwnedFd, RawFd},
        unix::net::{UnixListener, UnixStream},
    },
    path::Path,
    time::{Duration, Instant},
};

use nix::sys::socket::{recvmsg, sendmsg, ControlMessage, ControlMessageOwned, MsgFlags};

/// Sockets a process can hand over at once
const MAX_FDS: usize = 128;

/// Sends `listening` (each socket with its address) to the process waiting on `path`
///
/// # Errors
///
/// When no process waits on `path` (within a few seconds), or the sockets can't be sent.
pub fn send(path: &str, listening: &[(String, RawFd)]) -> std::io::Result<()> {
    let stream = connect(path, Duration::from_secs(5))?;
    let addresses = listening
        .iter()
        .map(|(address, _)| address.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let fds = listening
        .iter()
        .map(|(_, fd)| *fd)
        .take(MAX_FDS)
        .collect::<Vec<_>>();
    let payload = [IoSlice::new(addresses.as_bytes())];
    let rights = [ControlMessage::ScmRights(&fds)];
    sendmsg::<()>(
        stream.as_raw_fd(),
        &payload,
        &rights,
        MsgFlags::empty(),
        None,
    )?;
    Ok(())
}

/// Waits on `path` for the listening sockets of the process being replaced, at most `timeout`
///
/// # Errors
///
/// When none are sent in time, or they can't be read.
pub fn receive(path: &str, timeout: Duration) -> std::io::Result<Vec<(String, OwnedFd)>> {
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path)?;
    listener.set_nonblocking(true)?;
    let stream = accept(&listener, timeout, path)?;
    let _ = std::fs::remove_file(path);
    // The sender may be gone already (its sockets wait to be read): macOS then refuses changes
    // to the socket
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(timeout));

    let mut payload = vec![0u8; 64 * 1024];
    let mut space = nix::cmsg_space!([RawFd; MAX_FDS]);
    let mut iov = [IoSliceMut::new(&mut payload)];
    let message = recvmsg::<()>(
        stream.as_raw_fd(),
        &mut iov,
        Some(&mut space),
        MsgFlags::empty(),
    )?;
    let mut fds = vec![];
    for control in message.cmsgs()? {
        if let ControlMessageOwned::ScmRights(received) = control {
            // SAFETY: the kernel gave this process these descriptors, owned by nothing else
            fds.extend(
                received
                    .into_iter()
                    .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) }),
            );
        }
    }
    let read = message.bytes;
    let addresses = String::from_utf8_lossy(payload.get(..read).unwrap_or_default()).into_owned();
    Ok(addresses
        .split('\n')
        .filter(|address| !address.is_empty())
        .map(ToString::to_string)
        .zip(fds)
        .collect())
}

/// The process handing its sockets over, once it connects (within `timeout`)
fn accept(listener: &UnixListener, timeout: Duration, path: &str) -> std::io::Result<UnixStream> {
    let started = Instant::now();
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(err) if err.kind() != std::io::ErrorKind::WouldBlock => return Err(err),
            Err(_) if started.elapsed() >= timeout => {
                let _ = std::fs::remove_file(path);
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "no process handed its listening sockets over",
                ));
            }
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

fn connect(path: &str, timeout: Duration) -> std::io::Result<UnixStream> {
    let started = Instant::now();
    loop {
        match UnixStream::connect(Path::new(path)) {
            Ok(stream) => return Ok(stream),
            Err(err) if started.elapsed() >= timeout => return Err(err),
            Err(_) => std::thread::sleep(Duration::from_millis(50)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        os::fd::AsRawFd,
        time::Duration,
    };

    #[test]
    fn test_listening_sockets_are_handed_over() {
        // Short: Unix socket paths are limited to about a hundred bytes (less than macOS's
        // temporary directories)
        let path = format!("/tmp/super-transfer-{}.sock", std::process::id());

        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let receiving = {
            let path = path.clone();
            std::thread::spawn(move || super::receive(&path, Duration::from_secs(5)).unwrap())
        };
        super::send(&path, &[(address.clone(), listener.as_raw_fd())]).unwrap();
        let received = receiving.join().unwrap();
        drop(listener);

        let (handed, fd) = received.into_iter().next().unwrap();
        assert_eq!(handed, address);
        // The socket still listens, in its new owner
        let listener = std::net::TcpListener::from(fd);
        let mut client = std::net::TcpStream::connect(&address).unwrap();
        client.write_all(b"hi").unwrap();
        let (mut accepted, _) = listener.accept().unwrap();
        let mut read = [0u8; 2];
        accepted.read_exact(&mut read).unwrap();
        assert_eq!(&read, b"hi");
    }
}
