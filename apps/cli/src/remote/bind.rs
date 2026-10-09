//! Where the web UI listens: `127.0.0.1` and `[::1]` on one port (FP-104).
//!
//! `localhost` may resolve to either, so both are bound; a browser that
//! tries `::1` first must not land on some other program's server. Every
//! socket is exclusive (`SO_EXCLUSIVEADDRUSE`), so no program can bind the
//! same port beside this one. The last port is remembered, so the address a
//! person bookmarked keeps working across restarts.

use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::windows::io::AsRawSocket;
use std::path::Path;
use tokio::net::{TcpListener, TcpSocket};
use windows_sys::Win32::Networking::WinSock::{
    SO_EXCLUSIVEADDRUSE, SOCKET, SOL_SOCKET, setsockopt,
};

const FIRST_PORT: u16 = 47474;
const PORT_COUNT: u16 = 10;
/// Tries at an operating-system port whose twin on IPv6 was taken.
const SYSTEM_PORT_TRIES: usize = 5;
const PORT_FILE: &str = "web-port";
/// WSAEACCES: what Windows answers when another socket holds the port
/// exclusively.
const WSAEACCES: i32 = 10013;
/// WSAEAFNOSUPPORT and WSAEADDRNOTAVAIL: IPv6 (loopback) is not here.
const WSAEAFNOSUPPORT: i32 = 10047;
const WSAEADDRNOTAVAIL: i32 = 10049;

pub(super) struct Bound {
    pub(super) port: u16,
    pub(super) listeners: Vec<TcpListener>,
}

enum Tried {
    Busy,
    Failed(io::Error),
}

fn busy(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::AddrInUse || error.raw_os_error() == Some(WSAEACCES)
}

fn exclusive(address: SocketAddr) -> io::Result<TcpListener> {
    let socket = if address.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    let on: i32 = 1;
    // SAFETY: the handle is the open socket `socket` owns, and the option
    // value is a live i32 of the length given.
    let status = unsafe {
        setsockopt(
            socket.as_raw_socket() as SOCKET,
            SOL_SOCKET,
            SO_EXCLUSIVEADDRUSE,
            (&raw const on).cast(),
            size_of::<i32>() as i32,
        )
    };
    if status != 0 {
        return Err(io::Error::last_os_error());
    }
    socket.bind(address)?;
    socket.listen(128)
}

/// Both loopback addresses on `port` (0 for any free one). IPv4 alone only
/// when IPv6 loopback does not exist here at all; a busy IPv6 port moves
/// both on, and any other IPv6 failure fails the bind, so `[::1]` is never
/// left free for another program.
fn pair(port: u16) -> Result<Bound, Tried> {
    let v4 = exclusive((Ipv4Addr::LOCALHOST, port).into()).map_err(|error| {
        if busy(&error) {
            Tried::Busy
        } else {
            Tried::Failed(error)
        }
    })?;
    let port = v4.local_addr().map_err(Tried::Failed)?.port();
    let mut listeners = vec![v4];
    match exclusive((Ipv6Addr::LOCALHOST, port).into()) {
        Ok(v6) => listeners.push(v6),
        Err(error) if busy(&error) => return Err(Tried::Busy),
        // No IPv6 loopback on this machine: IPv4 alone is all there is.
        Err(error)
            if matches!(
                error.raw_os_error(),
                Some(WSAEAFNOSUPPORT | WSAEADDRNOTAVAIL)
            ) => {}
        Err(error) => return Err(Tried::Failed(error)),
    }
    Ok(Bound { port, listeners })
}

fn remembered(dir: &Path) -> Option<u16> {
    std::fs::read_to_string(dir.join(PORT_FILE))
        .ok()?
        .trim()
        .parse()
        .ok()
        .filter(|port| *port != 0)
}

fn remember(dir: &Path, port: u16) {
    if remembered(dir) == Some(port) {
        return;
    }
    // A port that cannot be remembered only means the next start may pick
    // another.
    let _ = (|| {
        let mut temp = tempfile::NamedTempFile::new_in(dir)?;
        io::Write::write_all(&mut temp, port.to_string().as_bytes())?;
        temp.persist(dir.join(PORT_FILE)).map_err(|e| e.error)?;
        io::Result::Ok(())
    })();
}

/// Binds the web UI's port. Run inside the listener's runtime.
pub(super) fn bind(dir: &Path) -> Result<Bound, String> {
    let first = remembered(dir);
    let candidates = first
        .into_iter()
        .chain((FIRST_PORT..FIRST_PORT + PORT_COUNT).filter(|port| Some(*port) != first));
    for port in candidates {
        match pair(port) {
            Ok(bound) => return Ok(done(dir, bound)),
            Err(Tried::Busy) => {}
            Err(Tried::Failed(error)) => return Err(unavailable(&error)),
        }
    }
    for _ in 0..SYSTEM_PORT_TRIES {
        match pair(0) {
            Ok(bound) => return Ok(done(dir, bound)),
            Err(Tried::Busy) => {}
            Err(Tried::Failed(error)) => return Err(unavailable(&error)),
        }
    }
    Err("No local port was free for the web UI.".into())
}

fn done(dir: &Path, bound: Bound) -> Bound {
    remember(dir, bound.port);
    bound
}

fn unavailable(error: &io::Error) -> String {
    format!("The web UI could not open its local port: {error}")
}
