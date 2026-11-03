//! The packet shim: a netfilter queue client that shapes marked connections.
//!
//! Needs `CAP_NET_ADMIN` (queue, iptables) and `CAP_NET_RAW` (to send SYN
//! retransmissions), and iptables rules that queue marked packets
//! ([`RuleGuard`]). It speaks `nfnetlink_queue` directly, so no
//! `libnetfilter_queue` is needed.
//!
//! What it does, per profile:
//! - rewrites the SYN (option layout, window, TTL, ECN; see `rewrite`);
//! - rewrites the IPv4 ID of every packet (`ip.id`);
//! - replaces the kernel's SYN retransmissions with the profile's schedule
//!   (`syn_retransmit`): it drops the kernel's and sends its own through a raw
//!   socket until the handshake ACK passes through, which ends the attempt.
//!
//! **Fails closed.** The rules have no `--queue-bypass`: if the shim stops,
//! marked packets are dropped and connections time out, instead of leaking the
//! host's real TCP stack. Unknown slots and unparsable SYNs are dropped too.

use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap};
use std::io;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU16, Ordering};
use std::sync::{Arc, Condvar, Mutex, MutexGuard};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use super::profile::{IdPolicy, TcpProfile};
use super::rewrite::{
    FlowKey, Kind, Outcome, classify, retransmit_syn, rewrite_syn, set_ipv4_id, tcp_seq,
};
use super::{MARK_ALL, MARK_BASE, MARK_MASK};

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
const COMMENT: &str = "lightbrowser-tcp-shim";
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
    /// SYNs rewritten.
    pub rewritten: u64,
    /// Accepted unchanged (not a client TCP packet, or unmarked).
    pub passed: u64,
    /// Dropped: unknown slot or a packet we could not parse.
    pub dropped: u64,
    /// Rewrites where the profile asked for something the kernel state does
    /// not allow (see `rewrite::Limit`).
    pub limited: u64,
    /// Netlink receive buffer overruns (packets lost to the kernel's policy).
    pub overruns: u64,
    /// Packets whose IPv4 ID was set by the profile's policy.
    pub id_rewritten: u64,
    /// Kernel SYN retransmissions dropped in favour of the profile's schedule.
    pub kernel_retransmits_dropped: u64,
    /// SYN retransmissions the shim sent itself.
    pub retransmits_sent: u64,
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

/// A connection attempt whose SYN retransmissions the shim controls.
struct Conn {
    /// The kernel's original SYN (retransmissions are built from it).
    kernel_syn: Vec<u8>,
    seq: u32,
    slot: u16,
    first_sent: Instant,
}

/// Sends hand-built SYNs, bypassing the marked-packet queue (the raw socket
/// has mark 0).
struct RawSender {
    v4: Option<OwnedFd>,
    v6: Option<OwnedFd>,
}

impl RawSender {
    fn open() -> Self {
        let sock = |family| {
            // SAFETY: plain socket(2) call; the result is checked.
            let fd = unsafe {
                libc::socket(
                    family,
                    libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                    libc::IPPROTO_RAW,
                )
            };
            // SAFETY: `fd` is a fresh descriptor that nothing else owns.
            (fd >= 0).then(|| unsafe { OwnedFd::from_raw_fd(fd) })
        };
        Self {
            v4: sock(libc::AF_INET),
            v6: sock(libc::AF_INET6),
        }
    }

    fn send(&self, dst: IpAddr, pkt: &[u8]) -> io::Result<()> {
        let unavailable = || io::Error::other("no raw socket (needs CAP_NET_RAW)");
        let n = match dst {
            IpAddr::V4(a) => {
                let fd = self.v4.as_ref().ok_or_else(unavailable)?;
                let sa = libc::sockaddr_in {
                    sin_family: libc::AF_INET as libc::sa_family_t,
                    sin_port: 0,
                    sin_addr: libc::in_addr {
                        s_addr: u32::from_ne_bytes(a.octets()),
                    },
                    sin_zero: [0; 8],
                };
                // SAFETY: `pkt` and `sa` are live for the call and their sizes
                // are passed with them.
                unsafe {
                    libc::sendto(
                        fd.as_raw_fd(),
                        pkt.as_ptr().cast(),
                        pkt.len(),
                        0,
                        std::ptr::from_ref(&sa).cast(),
                        std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
                    )
                }
            }
            IpAddr::V6(a) => {
                let fd = self.v6.as_ref().ok_or_else(unavailable)?;
                // SAFETY: sockaddr_in6 is plain data; all-zero is a valid value.
                let mut sa: libc::sockaddr_in6 = unsafe { std::mem::zeroed() };
                sa.sin6_family = libc::AF_INET6 as libc::sa_family_t;
                sa.sin6_addr = libc::in6_addr {
                    s6_addr: a.octets(),
                };
                // SAFETY: as above.
                unsafe {
                    libc::sendto(
                        fd.as_raw_fd(),
                        pkt.as_ptr().cast(),
                        pkt.len(),
                        0,
                        std::ptr::from_ref(&sa).cast(),
                        std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
                    )
                }
            }
        };
        if n < 0 { last_err() } else { Ok(()) }
    }
}

/// State shared by the packet loop and the retransmission timer.
struct Shared {
    by_slot: HashMap<u16, TcpProfile>,
    /// One IP ID counter per slot, for `IdPolicy::HostCounter`.
    counters: HashMap<u16, AtomicU16>,
    conns: Mutex<HashMap<FlowKey, Conn>>,
    /// Pending retransmissions: (when, connection, attempt number).
    timers: Mutex<BinaryHeap<Reverse<(Instant, FlowKey, u8)>>>,
    wake: Condvar,
    raw: RawSender,
    stats: Mutex<Stats>,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}

fn random_u16() -> u16 {
    use std::io::Read;
    let mut b = [0u8; 2];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_err()
    {
        // Not fingerprint data derived from the persona; fall back to the clock.
        return std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0x1234, |d| d.subsec_nanos() as u16);
    }
    u16::from_ne_bytes(b)
}

/// A bound netfilter queue plus the profiles it serves.
pub struct Shim {
    fd: OwnedFd,
    queue: u16,
    shared: Shared,
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
        let raw = RawSender::open();
        if profiles.iter().any(|p| p.syn_retransmit.is_some()) && raw.v4.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "SYN retransmission needs a raw socket (CAP_NET_RAW)",
            ));
        }
        let shim = Self {
            fd,
            queue,
            shared: Shared {
                counters: profiles
                    .iter()
                    .map(|p| (p.slot, AtomicU16::new(random_u16())))
                    .collect(),
                by_slot: profiles.into_iter().map(|p| (p.slot, p)).collect(),
                conns: Mutex::new(HashMap::new()),
                timers: Mutex::new(BinaryHeap::new()),
                wake: Condvar::new(),
                raw,
                stats: Mutex::new(Stats::default()),
            },
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

    fn count(&self, f: impl FnOnce(&mut Stats)) {
        f(&mut lock(&self.shared.stats));
    }

    /// The IP ID the profile's policy gives the next packet, if it sets one.
    fn next_id(&self, p: &TcpProfile) -> Option<u16> {
        match p.ip.id {
            IdPolicy::Kernel => None,
            IdPolicy::Zero => Some(0),
            IdPolicy::HostCounter => self
                .shared
                .counters
                .get(&p.slot)
                .map(|c| c.fetch_add(1, Ordering::Relaxed)),
        }
    }

    /// Decide what to do with one queued packet: a verdict and, if the packet
    /// changed, its new bytes.
    fn handle(&self, mark: u32, pkt: &[u8]) -> (u32, Option<Vec<u8>>) {
        let sh = &self.shared;
        if mark & MARK_MASK != MARK_BASE {
            self.count(|s| s.passed += 1);
            return (NF_ACCEPT, None);
        }
        let slot = (mark & (MARK_ALL - 1)) as u16;
        let Some(profile) = sh.by_slot.get(&slot) else {
            tracing::warn!(
                mark = format_args!("{mark:#x}"),
                "no TCP profile for this slot; dropping"
            );
            self.count(|s| s.dropped += 1);
            return (NF_DROP, None);
        };
        let Some((key, kind)) = classify(pkt) else {
            self.count(|s| s.passed += 1);
            return (NF_ACCEPT, None);
        };
        let mut out: Option<Vec<u8>> = None;
        match kind {
            Kind::Syn => {
                let seq = tcp_seq(pkt).unwrap_or(0);
                let mut conns = lock(&sh.conns);
                let retransmit = conns.get(&key).is_some_and(|c| c.seq == seq);
                if retransmit && profile.syn_retransmit.is_some() {
                    drop(conns);
                    self.count(|s| s.kernel_retransmits_dropped += 1);
                    return (NF_DROP, None);
                }
                let mut owned = pkt.to_vec();
                match rewrite_syn(&mut owned, profile) {
                    Ok(Outcome::Rewritten(report)) => {
                        if !report.limits.is_empty() {
                            tracing::warn!(profile = %profile.profile_id, ?report.limits, "SYN rewrite limited");
                        }
                        self.count(|s| {
                            s.rewritten += 1;
                            s.limited += u64::from(!report.limits.is_empty());
                        });
                        out = Some(owned);
                    }
                    Ok(Outcome::Untouched) => {}
                    Err(e) => {
                        tracing::warn!(error = %e, "cannot rewrite SYN; dropping");
                        self.count(|s| s.dropped += 1);
                        return (NF_DROP, None);
                    }
                }
                if let Some(r) = &profile.syn_retransmit {
                    let first_sent = Instant::now();
                    conns.insert(
                        key,
                        Conn {
                            kernel_syn: pkt.to_vec(),
                            seq,
                            slot,
                            first_sent,
                        },
                    );
                    drop(conns);
                    let mut timers = lock(&sh.timers);
                    for (i, ms) in r.schedule_ms.iter().enumerate() {
                        let at = first_sent + Duration::from_millis(u64::from(*ms));
                        timers.push(Reverse((at, key, (i + 1) as u8)));
                    }
                    drop(timers);
                    sh.wake.notify_one();
                }
            }
            // The first packet after the SYN is the handshake ACK: the attempt
            // is over, so no more retransmissions.
            Kind::Other | Kind::Closing => {
                if profile.syn_retransmit.is_some() {
                    lock(&sh.conns).remove(&key);
                }
            }
        }
        if let Some(id) = self.next_id(profile) {
            // IPv6 has no ID field; leave those packets as they are.
            let is_v4 = pkt.first().is_some_and(|b| b >> 4 == 4);
            if is_v4 {
                let buf = out.get_or_insert_with(|| pkt.to_vec());
                if set_ipv4_id(buf, id) {
                    self.count(|s| s.id_rewritten += 1);
                }
            }
        }
        if out.is_none() {
            self.count(|s| s.passed += 1);
        }
        (NF_ACCEPT, out)
    }

    /// Send the retransmission `attempt` of the connection `key`, if it is
    /// still waiting for its handshake.
    fn fire(&self, key: FlowKey, attempt: u8) {
        let sh = &self.shared;
        let (syn, slot, elapsed) = {
            let conns = lock(&sh.conns);
            let Some(c) = conns.get(&key) else { return };
            (c.kernel_syn.clone(), c.slot, c.first_sent.elapsed())
        };
        let Some(profile) = sh.by_slot.get(&slot) else {
            return;
        };
        let mut pkt = match retransmit_syn(&syn, profile, attempt, elapsed.as_millis() as u32) {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(error = %e, "cannot build the SYN retransmission");
                return;
            }
        };
        if let Some(id) = self.next_id(profile) {
            set_ipv4_id(&mut pkt, id);
        }
        match sh.raw.send(key.dst, &pkt) {
            Ok(()) => self.count(|s| s.retransmits_sent += 1),
            Err(e) => tracing::warn!(error = %e, "cannot send the SYN retransmission"),
        }
    }

    /// Sends scheduled retransmissions until `done` is set, and forgets
    /// attempts that can no longer be retransmitted.
    fn run_timers(&self, done: &AtomicBool) {
        let sh = &self.shared;
        while !done.load(Ordering::Relaxed) {
            let due = {
                let mut heap = lock(&sh.timers);
                let now = Instant::now();
                match heap.peek() {
                    Some(Reverse((at, ..))) if *at <= now => heap.pop().map(|Reverse(t)| t),
                    Some(Reverse((at, ..))) => {
                        let w = (*at - now).min(Duration::from_millis(200));
                        let _ = sh.wake.wait_timeout(heap, w);
                        None
                    }
                    None => {
                        let _ = sh.wake.wait_timeout(heap, Duration::from_millis(200));
                        None
                    }
                }
            };
            if let Some((_, key, attempt)) = due {
                self.fire(key, attempt);
            }
            // Keep an attempt a little past its last scheduled SYN, so the
            // kernel's later retransmissions are still recognised and dropped.
            lock(&sh.conns).retain(|_, c| {
                let last = sh
                    .by_slot
                    .get(&c.slot)
                    .and_then(|p| p.syn_retransmit.as_ref())
                    .and_then(|r| r.schedule_ms.last().copied())
                    .unwrap_or(0);
                c.first_sent.elapsed()
                    < Duration::from_millis(u64::from(last)) + Duration::from_secs(30)
            });
        }
    }

    /// Serve the queue until `stop` is set.
    pub fn run(&self, stop: &AtomicBool) -> io::Result<Stats> {
        let done = AtomicBool::new(false);
        let result = std::thread::scope(|scope| {
            scope.spawn(|| self.run_timers(&done));
            let r = self.run_queue(stop);
            done.store(true, Ordering::Relaxed);
            self.shared.wake.notify_all();
            r
        });
        let local = result?;
        let mut total = lock(&self.shared.stats).clone();
        total.seen += local.seen;
        total.overruns += local.overruns;
        Ok(total)
    }

    fn run_queue(&self, stop: &AtomicBool) -> io::Result<Stats> {
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
                        let (v, new) = self.handle(mark, pkt);
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
    /// One rule. `every_packet` queues all packets of connections marked with
    /// [`MARK_ALL`]; otherwise only the SYNs of connections without it.
    fn args(action: &str, queue: u16, every_packet: bool) -> Vec<String> {
        let (syn, mark) = if every_packet {
            (
                None,
                format!("{:#x}/{:#x}", MARK_BASE | MARK_ALL, MARK_MASK | MARK_ALL),
            )
        } else {
            (
                Some("--syn"),
                format!("{MARK_BASE:#x}/{:#x}", MARK_MASK | MARK_ALL),
            )
        };
        ["-t", "mangle", action, "OUTPUT", "-p", "tcp"]
            .into_iter()
            .chain(syn)
            .chain([
                "-m",
                "mark",
                "--mark",
                &mark,
                "-m",
                "comment",
                "--comment",
                COMMENT,
            ])
            .chain(["-j", "NFQUEUE", "--queue-num", &queue.to_string()])
            .map(String::from)
            .collect()
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

    /// Install the IPv4 rules (required) and the IPv6 rules (best effort).
    pub fn install(queue: u16) -> io::Result<Self> {
        let mut rules = Vec::new();
        for (tool, required) in [("iptables", true), ("ip6tables", false)] {
            for every_packet in [false, true] {
                match Self::run(tool, &Self::args("-I", queue, every_packet)) {
                    Ok(()) => rules.push((tool, Self::args("-D", queue, every_packet))),
                    Err(e) if required => return Err(e),
                    Err(e) => {
                        tracing::warn!(error = %e, "no IPv6 rule; IPv6 packets will not be shaped");
                    }
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
    let n = text
        .lines()
        .filter(|l| l.contains(COMMENT) && l.contains("NFQUEUE"))
        .count();
    Ok(n >= 2)
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
