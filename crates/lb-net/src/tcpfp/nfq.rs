//! The packet shim: a netfilter queue client that rewrites marked SYNs.
//!
//! Needs `CAP_NET_ADMIN` and an iptables rule that queues marked SYNs
//! ([`RuleGuard`]). It speaks `nfnetlink_queue` directly, so no
//! `libnetfilter_queue` is needed.
//!
//! **Fails closed.** The rule has no `--queue-bypass`: if the shim stops,
//! marked SYNs are dropped and connections time out, instead of leaking the
//! host's real TCP stack. Unknown slots and unparsable packets are dropped
//! too.

use std::collections::HashMap;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;

use super::profile::TcpProfile;
use super::rewrite::{Outcome, rewrite_syn};
use super::{MARK_BASE, MARK_MASK};

const NETLINK_NETFILTER: libc::c_int = 12;
const NFNL_SUBSYS_QUEUE: u16 = 3;
const NFQNL_MSG_PACKET: u16 = 0;
const NFQNL_MSG_VERDICT: u16 = 1;
const NFQNL_MSG_CONFIG: u16 = 2;
const NFQA_PACKET_HDR: u16 = 1;
const NFQA_VERDICT_HDR: u16 = 2;
const NFQA_MARK: u16 = 3;
const NFQA_PAYLOAD: u16 = 10;
const NFQA_CFG_CMD: u16 = 1;
const NFQA_CFG_PARAMS: u16 = 2;
const NFQNL_CFG_CMD_BIND: u8 = 1;
const NFQNL_COPY_PACKET: u8 = 2;
const NF_DROP: u32 = 0;
const NF_ACCEPT: u32 = 1;
const NLM_F_REQUEST: u16 = 1;
const NLM_F_ACK: u16 = 4;
const NLMSG_ERROR: u16 = 2;
const NLMSG_HDR: usize = 16;
const NFGEN_HDR: usize = 4;

/// Counters for one shim run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub seen: u64,
    pub rewritten: u64,
    /// Accepted unchanged (not a client SYN, or unmarked).
    pub passed: u64,
    /// Dropped: unknown slot or a packet we could not parse.
    pub dropped: u64,
    /// Rewrites where the profile asked for something the kernel state does
    /// not allow (see `rewrite::Limit`).
    pub limited: u64,
    /// Netlink receive buffer overruns (packets lost to the kernel's policy).
    pub overruns: u64,
}

fn align4(n: usize) -> usize {
    (n + 3) & !3
}

/// One netlink message: header, nfgenmsg and attributes.
fn message(typ: u16, flags: u16, seq: u32, queue: u16, attrs: &[(u16, &[u8])]) -> Vec<u8> {
    let mut m = vec![0u8; NLMSG_HDR];
    m.extend([libc::AF_UNSPEC as u8, 0]); // nfgen_family, version
    m.extend(queue.to_be_bytes());
    for (kind, data) in attrs {
        m.extend(((4 + data.len()) as u16).to_ne_bytes());
        m.extend(kind.to_ne_bytes());
        m.extend(*data);
        m.resize(align4(m.len()), 0);
    }
    let len = m.len() as u32;
    m[0..4].copy_from_slice(&len.to_ne_bytes());
    m[4..6].copy_from_slice(&((NFNL_SUBSYS_QUEUE << 8) | typ).to_ne_bytes());
    m[6..8].copy_from_slice(&flags.to_ne_bytes());
    m[8..12].copy_from_slice(&seq.to_ne_bytes());
    m
}

/// The attributes of a netlink message body (after nfgenmsg).
fn attrs(mut b: &[u8]) -> impl Iterator<Item = (u16, &[u8])> {
    std::iter::from_fn(move || {
        if b.len() < 4 {
            return None;
        }
        let len = usize::from(u16::from_ne_bytes([b[0], b[1]]));
        let kind = u16::from_ne_bytes([b[2], b[3]]) & 0x3fff;
        if len < 4 || len > b.len() {
            return None;
        }
        let data = &b[4..len];
        b = &b[align4(len).min(b.len())..];
        Some((kind, data))
    })
}

fn last_err<T>() -> io::Result<T> {
    Err(io::Error::last_os_error())
}

/// A bound netfilter queue plus the profiles it serves.
pub struct Shim {
    fd: OwnedFd,
    queue: u16,
    by_slot: HashMap<u16, TcpProfile>,
}

impl Shim {
    /// Bind queue `queue` and serve `profiles`, keyed by their slot.
    pub fn bind(queue: u16, profiles: Vec<TcpProfile>) -> io::Result<Self> {
        // SAFETY: plain socket(2) call; the result is checked below.
        let raw = unsafe {
            libc::socket(
                libc::AF_NETLINK,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                NETLINK_NETFILTER,
            )
        };
        if raw < 0 {
            return last_err();
        }
        // SAFETY: `raw` is a fresh descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: sockaddr_nl is plain data; all-zero is a valid value.
        let mut addr: libc::sockaddr_nl = unsafe { std::mem::zeroed() };
        addr.nl_family = libc::AF_NETLINK as u16;
        // SAFETY: `addr` is live and its size is passed with it.
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                std::ptr::from_ref(&addr).cast(),
                std::mem::size_of::<libc::sockaddr_nl>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return last_err();
        }
        // Wake up every 200 ms so `run` can see the stop flag.
        let tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 200_000,
        };
        // SAFETY: `tv` is live for the call and its size is passed with it.
        let r = unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&tv).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return last_err();
        }
        let shim = Self {
            fd,
            queue,
            by_slot: profiles.into_iter().map(|p| (p.slot, p)).collect(),
        };
        // BIND (pf is ignored by modern kernels), then copy whole packets.
        let bind = [NFQNL_CFG_CMD_BIND, 0, 0, 0];
        shim.send(&message(
            NFQNL_MSG_CONFIG,
            NLM_F_REQUEST | NLM_F_ACK,
            1,
            queue,
            &[(NFQA_CFG_CMD, &bind)],
        ))?;
        shim.expect_ack()?;
        let mut params = 0xffff_u32.to_be_bytes().to_vec();
        params.push(NFQNL_COPY_PACKET);
        shim.send(&message(
            NFQNL_MSG_CONFIG,
            NLM_F_REQUEST | NLM_F_ACK,
            2,
            queue,
            &[(NFQA_CFG_PARAMS, &params)],
        ))?;
        shim.expect_ack()?;
        Ok(shim)
    }

    fn send(&self, msg: &[u8]) -> io::Result<()> {
        // SAFETY: `msg` is a live slice; the kernel copies it during the call.
        let n = unsafe { libc::send(self.fd.as_raw_fd(), msg.as_ptr().cast(), msg.len(), 0) };
        if n < 0 { last_err() } else { Ok(()) }
    }

    fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `buf` is a live, writable slice of the length passed.
        let n = unsafe { libc::recv(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len(), 0) };
        if n < 0 { last_err() } else { Ok(n as usize) }
    }

    fn expect_ack(&self) -> io::Result<()> {
        let mut buf = [0u8; 512];
        let n = self.recv(&mut buf)?;
        if n >= NLMSG_HDR + 4 && u16::from_ne_bytes([buf[4], buf[5]]) == NLMSG_ERROR {
            let errno = i32::from_ne_bytes([buf[16], buf[17], buf[18], buf[19]]);
            if errno != 0 {
                return Err(io::Error::from_raw_os_error(-errno));
            }
        }
        Ok(())
    }

    fn verdict(&self, id: u32, verdict: u32, payload: Option<&[u8]>) -> io::Result<()> {
        let mut hdr = verdict.to_be_bytes().to_vec();
        hdr.extend(id.to_be_bytes());
        let mut list: Vec<(u16, &[u8])> = vec![(NFQA_VERDICT_HDR, &hdr)];
        if let Some(p) = payload {
            list.push((NFQA_PAYLOAD, p));
        }
        self.send(&message(
            NFQNL_MSG_VERDICT,
            NLM_F_REQUEST,
            0,
            self.queue,
            &list,
        ))
    }

    /// Decide what to do with one queued packet.
    fn handle(&self, mark: u32, pkt: &[u8], stats: &mut Stats) -> (u32, Option<Vec<u8>>) {
        if mark & MARK_MASK != MARK_BASE {
            stats.passed += 1;
            return (NF_ACCEPT, None);
        }
        let Some(profile) = self.by_slot.get(&((mark & 0xffff) as u16)) else {
            tracing::warn!(
                mark = format_args!("{mark:#x}"),
                "no TCP profile for this slot; dropping"
            );
            stats.dropped += 1;
            return (NF_DROP, None);
        };
        let mut owned = pkt.to_vec();
        match rewrite_syn(&mut owned, profile) {
            Ok(Outcome::Rewritten(report)) => {
                if !report.limits.is_empty() {
                    stats.limited += 1;
                    tracing::warn!(profile = %profile.profile_id, ?report.limits, "SYN rewrite limited");
                }
                stats.rewritten += 1;
                (NF_ACCEPT, Some(owned))
            }
            Ok(Outcome::Untouched) => {
                stats.passed += 1;
                (NF_ACCEPT, None)
            }
            Err(e) => {
                tracing::warn!(error = %e, "cannot rewrite SYN; dropping");
                stats.dropped += 1;
                (NF_DROP, None)
            }
        }
    }

    /// Serve the queue until `stop` is set.
    pub fn run(&self, stop: &AtomicBool) -> io::Result<Stats> {
        let mut stats = Stats::default();
        let mut buf = vec![0u8; 1 << 17];
        while !stop.load(Ordering::Relaxed) {
            let n = match self.recv(&mut buf) {
                Ok(n) => n,
                Err(e) => match e.raw_os_error() {
                    Some(libc::EAGAIN | libc::EINTR) => continue,
                    Some(libc::ENOBUFS) => {
                        stats.overruns += 1;
                        continue;
                    }
                    _ => return Err(e),
                },
            };
            let mut off = 0;
            while off + NLMSG_HDR <= n {
                let len = u32::from_ne_bytes([buf[off], buf[off + 1], buf[off + 2], buf[off + 3]])
                    as usize;
                if len < NLMSG_HDR || off + len > n {
                    break;
                }
                let typ = u16::from_ne_bytes([buf[off + 4], buf[off + 5]]);
                if typ == (NFNL_SUBSYS_QUEUE << 8) | NFQNL_MSG_PACKET
                    && len >= NLMSG_HDR + NFGEN_HDR
                {
                    let body = &buf[off + NLMSG_HDR + NFGEN_HDR..off + len];
                    let (mut id, mut mark, mut payload) = (None, 0u32, None);
                    for (kind, data) in attrs(body) {
                        match kind {
                            NFQA_PACKET_HDR if data.len() >= 4 => {
                                id = Some(u32::from_be_bytes([data[0], data[1], data[2], data[3]]));
                            }
                            NFQA_MARK if data.len() >= 4 => {
                                mark = u32::from_be_bytes([data[0], data[1], data[2], data[3]]);
                            }
                            NFQA_PAYLOAD => payload = Some(data),
                            _ => {}
                        }
                    }
                    if let (Some(id), Some(pkt)) = (id, payload) {
                        stats.seen += 1;
                        let (v, new) = self.handle(mark, pkt, &mut stats);
                        self.verdict(id, v, new.as_deref())?;
                    }
                }
                off += align4(len);
            }
        }
        Ok(stats)
    }

    /// Run on a new thread. Call [`ShimHandle::stop`] to end it.
    pub fn spawn(self) -> ShimHandle {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let join = std::thread::Builder::new()
            .name("lb-tcp-shim".into())
            .spawn(move || self.run(&flag))
            .expect("spawn shim thread");
        ShimHandle { stop, join }
    }
}

pub struct ShimHandle {
    stop: Arc<AtomicBool>,
    join: JoinHandle<io::Result<Stats>>,
}

impl ShimHandle {
    pub fn stop(self) -> io::Result<Stats> {
        self.stop.store(true, Ordering::Relaxed);
        self.join
            .join()
            .map_err(|_| io::Error::other("shim thread panicked"))?
    }
}

/// The iptables rules that send marked SYNs to the queue. Removed on drop.
///
/// `-j NFQUEUE` without `--queue-bypass` is deliberate: see the module docs.
pub struct RuleGuard {
    rules: Vec<(&'static str, Vec<String>)>,
}

impl RuleGuard {
    fn args(action: &str, queue: u16) -> Vec<String> {
        [
            "-t",
            "mangle",
            action,
            "OUTPUT",
            "-p",
            "tcp",
            "--syn",
            "-m",
            "mark",
            "--mark",
            &format!("{MARK_BASE:#x}/{MARK_MASK:#x}"),
            "-m",
            "comment",
            "--comment",
            "lightbrowser-tcp-shim",
            "-j",
            "NFQUEUE",
            "--queue-num",
            &queue.to_string(),
        ]
        .map(String::from)
        .to_vec()
    }

    fn run(tool: &str, args: &[String]) -> io::Result<()> {
        let out = Command::new(tool)
            .args(args)
            .stdout(Stdio::null())
            .output()?;
        if out.status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "{tool} {}: {}",
                args.join(" "),
                String::from_utf8_lossy(&out.stderr).trim()
            )))
        }
    }

    /// Install the IPv4 rule (required) and the IPv6 rule (best effort).
    pub fn install(queue: u16) -> io::Result<Self> {
        let mut rules = Vec::new();
        for (tool, required) in [("iptables", true), ("ip6tables", false)] {
            match Self::run(tool, &Self::args("-I", queue)) {
                Ok(()) => rules.push((tool, Self::args("-D", queue))),
                Err(e) if required => return Err(e),
                Err(e) => {
                    tracing::warn!(error = %e, "no IPv6 rule; IPv6 SYNs will not be rewritten")
                }
            }
        }
        Ok(Self { rules })
    }
}

/// Whether the IPv4 shim rule is installed. `Err` means iptables could not be
/// run, so the answer is unknown.
pub fn rule_installed() -> io::Result<bool> {
    let out = Command::new("iptables")
        .args(["-t", "mangle", "-S", "OUTPUT"])
        .stderr(Stdio::null())
        .output()?;
    if !out.status.success() {
        return Err(io::Error::other("iptables -S failed"));
    }
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .any(|l| l.contains("lightbrowser-tcp-shim") && l.contains("NFQUEUE")))
}

impl Drop for RuleGuard {
    fn drop(&mut self) {
        for (tool, args) in &self.rules {
            if let Err(e) = Self::run(tool, args) {
                tracing::warn!(error = %e, "could not remove the shim rule");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_message_layout_matches_the_kernel_abi() {
        let m = message(
            NFQNL_MSG_CONFIG,
            NLM_F_REQUEST | NLM_F_ACK,
            1,
            7,
            &[(NFQA_CFG_CMD, &[1, 0, 0, 0])],
        );
        assert_eq!(
            u32::from_ne_bytes(m[0..4].try_into().unwrap()) as usize,
            m.len()
        );
        assert_eq!(
            u16::from_ne_bytes(m[4..6].try_into().unwrap()),
            (3 << 8) | 2
        );
        assert_eq!(
            &m[16..20],
            [0, 0, 0, 7],
            "family, version, big-endian queue"
        );
        assert_eq!(
            u16::from_ne_bytes(m[20..22].try_into().unwrap()),
            8,
            "attr len"
        );
        assert_eq!(m.len(), 28);
    }

    #[test]
    fn attribute_walker_handles_padding() {
        let m = message(NFQNL_MSG_VERDICT, 0, 0, 1, &[(1, &[1, 2, 3]), (2, &[9; 4])]);
        let got: Vec<_> = attrs(&m[NLMSG_HDR + NFGEN_HDR..]).collect();
        assert_eq!(got, [(1, &[1u8, 2, 3][..]), (2, &[9u8; 4][..])]);
    }
}
