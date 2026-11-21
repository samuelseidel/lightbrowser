//! TCP/IP stack fingerprint: make a connection's first packet look like it
//! came from Windows, macOS or Linux (ADR 0031).
//!
//! Servers can fingerprint the operating system from the SYN alone: IP TTL,
//! DF bit, TCP window, and which options appear in which order (p0f, JA4T).
//! A Chrome-on-Windows persona that sends a Linux SYN contradicts itself.
//!
//! Two tiers, picked per profile:
//! - **Sockets** ([`prepare_socket`], unprivileged): TTL, MSS, window scale
//!   and so the window, set with `setsockopt` before `connect`. This is all a
//!   Linux persona needs.
//! - **Shim** ([`nfq`], needs `CAP_NET_ADMIN`): a netfilter queue process
//!   rewrites the SYN's option block for what sockets cannot change (option
//!   order, timestamps off). Connections opt in with `SO_MARK`, which also
//!   selects the profile, so personas with different operating systems can
//!   share one process.
//!
//! Everything comes from the context's [`TcpProfile`]; nothing is global.

pub mod profile;
pub mod rewrite;

#[cfg(target_os = "linux")]
pub mod nfq;

use std::io;
use std::net::SocketAddr;

use tokio::net::{TcpSocket, TcpStream};

pub use profile::{TcpOpt, TcpProfile};

/// Socket marks `MARK_BASE | slot` select a profile in the shim. The high 16
/// bits spell "LB" so other users of `SO_MARK` do not collide.
pub const MARK_BASE: u32 = 0x4c42_0000;
/// Mask that isolates [`MARK_BASE`] in a mark.
pub const MARK_MASK: u32 = 0xffff_0000;
/// Mark bit that sends every packet of the connection to the shim, not only
/// the SYN. The profile slot is the low 15 bits.
pub const MARK_ALL: u32 = 0x8000;

/// Set the per-socket knobs for `p` on a socket that has not connected yet.
#[cfg(target_os = "linux")]
pub fn prepare_socket(sock: &TcpSocket, v6: bool, p: &TcpProfile) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    let fd = sock.as_raw_fd();
    if v6 {
        set_int(
            fd,
            libc::IPPROTO_IPV6,
            libc::IPV6_UNICAST_HOPS,
            p.ip.ttl.into(),
        )?;
    } else {
        set_int(fd, libc::IPPROTO_IP, libc::IP_TTL, p.ip.ttl.into())?;
    }
    // The kernel advertises this MSS, and sizes the SYN window from it.
    set_int(fd, libc::IPPROTO_TCP, libc::TCP_MAXSEG, p.tcp.mss.into())?;
    // The kernel derives the window-scale shift from the clamp.
    set_int(
        fd,
        libc::IPPROTO_TCP,
        libc::TCP_WINDOW_CLAMP,
        p.window_clamp() as libc::c_int,
    )?;
    if p.needs_shim() {
        set_int(fd, libc::SOL_SOCKET, libc::SO_MARK, p.mark() as libc::c_int).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!(
                    "TCP profile {} needs the packet shim, and marking the socket needs \
                     CAP_NET_ADMIN: {e}",
                    p.profile_id
                ),
            )
        })?;
    }
    Ok(())
}

#[cfg(not(target_os = "linux"))]
pub fn prepare_socket(_: &TcpSocket, _: bool, p: &TcpProfile) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        format!("TCP profile {} needs Linux", p.profile_id),
    ))
}

#[cfg(target_os = "linux")]
fn set_int(
    fd: std::os::fd::RawFd,
    level: libc::c_int,
    name: libc::c_int,
    val: libc::c_int,
) -> io::Result<()> {
    // SAFETY: `fd` is an open socket that the caller keeps alive for this
    // call, and the pointer and length describe one live `c_int`.
    let r = unsafe {
        libc::setsockopt(
            fd,
            level,
            name,
            std::ptr::from_ref(&val).cast(),
            std::mem::size_of::<libc::c_int>() as libc::socklen_t,
        )
    };
    if r == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

/// Connect to `addr`. With a profile, the socket is prepared first, so the
/// SYN carries the profile's values. Without one this is a plain connect.
pub async fn connect(addr: SocketAddr, profile: Option<&TcpProfile>) -> io::Result<TcpStream> {
    let Some(p) = profile else {
        return TcpStream::connect(addr).await;
    };
    let sock = if addr.is_ipv4() {
        TcpSocket::new_v4()?
    } else {
        TcpSocket::new_v6()?
    };
    prepare_socket(&sock, addr.is_ipv6(), p)?;
    sock.connect(addr).await
}
