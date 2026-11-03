//! TCP/IP SYN capture and signature, for checking `lb-net`'s `tcpfp` layer
//! (ADR 0007). Linux only. Needs `CAP_NET_RAW`.
//!
//! The parser here shares no code with the rewriter in `lb-net`, so a test
//! cannot pass just because both sides make the same mistake. It also
//! verifies the IPv4 header and TCP checksums.
//!
//! Capture on loopback or on a lab interface to our own test server only.

use std::io;
use std::net::IpAddr;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use serde::Serialize;

/// A client SYN as seen on the wire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Syn {
    pub src: IpAddr,
    pub dst: IpAddr,
    pub src_port: u16,
    pub dst_port: u16,
    /// IPv4 TTL or IPv6 hop limit.
    pub ttl: u8,
    /// IPv4 DF bit (always false for IPv6).
    pub df: bool,
    pub ip_id: u16,
    pub flags: u8,
    pub window: u16,
    /// Option kinds in wire order. Parsing stops at the first EOL, which is
    /// kept as `0`. NOPs are kept as `1`.
    pub option_kinds: Vec<u8>,
    pub mss: Option<u16>,
    pub window_scale: Option<u8>,
    pub timestamps: Option<(u32, u32)>,
    /// Raw option bytes, for exact comparison.
    pub option_bytes: Vec<u8>,
    /// Both the IPv4 header checksum (if any) and the TCP checksum verify.
    ///
    /// Not meaningful for packets the capturing host's own kernel sent
    /// untouched: with checksum offload they carry only the pseudo-header
    /// seed. It is meaningful for packets rewritten by the shim (the kernel
    /// then treats the checksum as complete) and for packets received from
    /// another host.
    pub checksums_ok: bool,
}

impl Syn {
    /// JA4T-style value: `window_options_mss_wscale`.
    pub fn ja4t(&self) -> String {
        let kinds: Vec<String> = self.option_kinds.iter().map(u8::to_string).collect();
        format!(
            "{}_{}_{}_{}",
            self.window,
            kinds.join("-"),
            self.mss.map(|m| m.to_string()).unwrap_or_default(),
            self.window_scale.map(|w| w.to_string()).unwrap_or_default(),
        )
    }
}

fn sum16(data: &[u8], mut acc: u32) -> u32 {
    for c in data.chunks(2) {
        acc += (u32::from(c[0]) << 8) | c.get(1).map_or(0, |b| u32::from(*b));
    }
    acc
}

fn folded(mut acc: u32) -> u16 {
    while acc >> 16 != 0 {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    acc as u16
}

/// (kinds in wire order, MSS, window scale, timestamps). Stops at the first
/// EOL, which is kept as `0`; NOPs are kept as `1`. `None` if malformed.
type Options = (Vec<u8>, Option<u16>, Option<u8>, Option<(u32, u32)>);

fn parse_options(raw: &[u8]) -> Option<Options> {
    let (mut kinds, mut mss, mut ws, mut ts) = (vec![], None, None, None);
    let mut i = 0;
    while i < raw.len() {
        let k = raw[i];
        kinds.push(k);
        match k {
            0 => break,
            1 => i += 1,
            _ => {
                let l = usize::from(*raw.get(i + 1)?);
                if l < 2 || i + l > raw.len() {
                    return None;
                }
                let b = &raw[i + 2..i + l];
                match (k, b.len()) {
                    (2, 2) => mss = Some(u16::from_be_bytes([b[0], b[1]])),
                    (3, 1) => ws = Some(b[0]),
                    (8, 8) => {
                        ts = Some((
                            u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
                            u32::from_be_bytes([b[4], b[5], b[6], b[7]]),
                        ));
                    }
                    _ => {}
                }
                i += l;
            }
        }
    }
    Some((kinds, mss, ws, ts))
}

/// Parse an IP packet; `None` unless it is a client SYN (SYN set, ACK clear).
pub fn parse_syn(ip: &[u8]) -> Option<Syn> {
    let (src, dst, ttl, df, ip_id, tcp, mut ok, pseudo): (
        IpAddr,
        IpAddr,
        u8,
        bool,
        u16,
        &[u8],
        bool,
        u32,
    ) = match ip.first()? >> 4 {
        4 => {
            let ihl = usize::from(ip[0] & 15) * 4;
            let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
            if ihl < 20 || total > ip.len() || total < ihl || ip[9] != 6 {
                return None;
            }
            let s: [u8; 4] = ip[12..16].try_into().ok()?;
            let d: [u8; 4] = ip[16..20].try_into().ok()?;
            let tcp = &ip[ihl..total];
            let hdr_ok = folded(sum16(&ip[..ihl], 0)) == 0xffff;
            let pseudo = sum16(&s, 0) + sum16(&d, 0) + 6 + tcp.len() as u32;
            (
                s.into(),
                d.into(),
                ip[8],
                u16::from_be_bytes([ip[6], ip[7]]) & 0x4000 != 0,
                u16::from_be_bytes([ip[4], ip[5]]),
                tcp,
                hdr_ok,
                pseudo,
            )
        }
        6 => {
            if ip.len() < 40 || ip[6] != 6 {
                return None;
            }
            let n = usize::from(u16::from_be_bytes([ip[4], ip[5]]));
            if 40 + n > ip.len() {
                return None;
            }
            let s: [u8; 16] = ip[8..24].try_into().ok()?;
            let d: [u8; 16] = ip[24..40].try_into().ok()?;
            let tcp = &ip[40..40 + n];
            let pseudo = sum16(&s, 0) + sum16(&d, 0) + n as u32 + 6;
            (s.into(), d.into(), ip[7], false, 0, tcp, true, pseudo)
        }
        _ => return None,
    };
    if tcp.len() < 20 || tcp[13] & 0x12 != 0x02 {
        return None;
    }
    ok &= folded(sum16(tcp, pseudo)) == 0xffff;
    let hlen = usize::from(tcp[12] >> 4) * 4;
    if hlen < 20 || hlen > tcp.len() {
        return None;
    }
    let raw = &tcp[20..hlen];
    let (kinds, mss, ws, ts) = parse_options(raw)?;
    Some(Syn {
        src,
        dst,
        src_port: u16::from_be_bytes([tcp[0], tcp[1]]),
        dst_port: u16::from_be_bytes([tcp[2], tcp[3]]),
        ttl,
        df,
        ip_id,
        flags: tcp[13],
        window: u16::from_be_bytes([tcp[14], tcp[15]]),
        option_kinds: kinds,
        mss,
        window_scale: ws,
        timestamps: ts,
        option_bytes: raw.to_vec(),
        checksums_ok: ok,
    })
}

/// A client TCP packet as seen on the wire, any kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pkt {
    /// Milliseconds since the first packet of the capture.
    pub at_ms: u64,
    pub src_port: u16,
    pub ttl: u8,
    /// IPv4 identification (0 for IPv6).
    pub ip_id: u16,
    pub flags: u8,
    pub window: u16,
    pub seq: u32,
    pub payload_len: usize,
    pub option_kinds: Vec<u8>,
    pub window_scale: Option<u8>,
    pub timestamps: Option<(u32, u32)>,
}

impl Pkt {
    pub fn is_syn(&self) -> bool {
        self.flags & 0x12 == 0x02
    }
}

/// Parse any TCP packet (IPv4, or IPv6 without extension headers).
pub fn parse_packet(ip: &[u8], at_ms: u64) -> Option<Pkt> {
    let (ttl, ip_id, tcp) = match ip.first()? >> 4 {
        4 => {
            let ihl = usize::from(ip[0] & 15) * 4;
            let total = usize::from(u16::from_be_bytes([ip[2], ip[3]]));
            if ihl < 20 || total > ip.len() || total < ihl + 20 || ip[9] != 6 {
                return None;
            }
            (ip[8], u16::from_be_bytes([ip[4], ip[5]]), &ip[ihl..total])
        }
        6 => {
            let n = usize::from(u16::from_be_bytes([
                ip.get(4).copied()?,
                ip.get(5).copied()?,
            ]));
            if ip.len() < 40 + n || n < 20 || ip[6] != 6 {
                return None;
            }
            (ip[7], 0, &ip[40..40 + n])
        }
        _ => return None,
    };
    let hlen = usize::from(tcp[12] >> 4) * 4;
    if hlen < 20 || hlen > tcp.len() {
        return None;
    }
    let (kinds, _, ws, ts) = parse_options(&tcp[20..hlen])?;
    Some(Pkt {
        at_ms,
        src_port: u16::from_be_bytes([tcp[0], tcp[1]]),
        ttl,
        ip_id,
        flags: tcp[13],
        window: u16::from_be_bytes([tcp[14], tcp[15]]),
        seq: u32::from_be_bytes([tcp[4], tcp[5], tcp[6], tcp[7]]),
        payload_len: tcp.len() - hlen,
        option_kinds: kinds,
        window_scale: ws,
        timestamps: ts,
    })
}

/// A packet as captured: when, and the IP bytes.
#[derive(Debug, Clone)]
pub struct Raw {
    pub at: Instant,
    pub ip: Vec<u8>,
}

/// The client SYNs in `raw`.
pub fn syns(raw: &[Raw]) -> Vec<Syn> {
    raw.iter().filter_map(|r| parse_syn(&r.ip)).collect()
}

/// Every client TCP packet in `raw`, with times relative to the first.
pub fn packets(raw: &[Raw]) -> Vec<Pkt> {
    let Some(t0) = raw.first().map(|r| r.at) else {
        return Vec::new();
    };
    raw.iter()
        .filter_map(|r| parse_packet(&r.ip, r.at.duration_since(t0).as_millis() as u64))
        .collect()
}

/// Captures the packets sent to one destination port on one interface, each
/// once. On `lo` that is the outgoing copy; elsewhere, what arrives.
pub struct Capture {
    stop: Arc<AtomicBool>,
    join: JoinHandle<(Vec<Raw>, u32)>,
}

impl Capture {
    /// Start capturing on `iface` (e.g. `lo`). The socket is open when this
    /// returns, so a connect right after it is seen.
    pub fn start(iface: &str, dst_port: u16) -> io::Result<Self> {
        let name = std::ffi::CString::new(iface).map_err(io::Error::other)?;
        // SAFETY: `name` is a valid NUL-terminated string.
        let ifindex = unsafe { libc::if_nametoindex(name.as_ptr()) };
        if ifindex == 0 {
            return Err(io::Error::last_os_error());
        }
        let loopback = iface == "lo";
        // SAFETY: plain socket(2) call; the result is checked below.
        let raw = unsafe {
            libc::socket(
                libc::AF_PACKET,
                libc::SOCK_RAW | libc::SOCK_CLOEXEC,
                i32::from((libc::ETH_P_ALL as u16).to_be()),
            )
        };
        if raw < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `raw` is a fresh descriptor that nothing else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: sockaddr_ll is plain data; all-zero is a valid value.
        let mut sa: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
        sa.sll_family = libc::AF_PACKET as u16;
        sa.sll_protocol = (libc::ETH_P_ALL as u16).to_be();
        sa.sll_ifindex = ifindex as i32;
        // SAFETY: `sa` is live and its size is passed with it.
        let r = unsafe {
            libc::bind(
                fd.as_raw_fd(),
                std::ptr::from_ref(&sa).cast(),
                std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t,
            )
        };
        if r < 0 {
            return Err(io::Error::last_os_error());
        }
        // Loopback carries the whole response too; a small buffer would lose
        // packets we need. FORCE ignores rmem_max (we run with CAP_NET_ADMIN).
        let big: libc::c_int = 64 << 20;
        // SAFETY: `big` is live for the call and its size is passed with it.
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVBUFFORCE,
                std::ptr::from_ref(&big).cast(),
                std::mem::size_of::<libc::c_int>() as libc::socklen_t,
            );
        }
        let tv = libc::timeval {
            tv_sec: 0,
            tv_usec: 50_000,
        };
        // SAFETY: `tv` is live for the call and its size is passed with it.
        unsafe {
            libc::setsockopt(
                fd.as_raw_fd(),
                libc::SOL_SOCKET,
                libc::SO_RCVTIMEO,
                std::ptr::from_ref(&tv).cast(),
                std::mem::size_of::<libc::timeval>() as libc::socklen_t,
            );
        }
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let join = std::thread::spawn(move || {
            let mut out: Vec<Raw> = Vec::new();
            let mut buf = vec![0u8; 65536];
            while !flag.load(Ordering::Relaxed) {
                // SAFETY: sockaddr_ll is plain data; all-zero is a valid value.
                let mut from: libc::sockaddr_ll = unsafe { std::mem::zeroed() };
                let mut from_len = std::mem::size_of::<libc::sockaddr_ll>() as libc::socklen_t;
                // SAFETY: `buf` and `from` are live and writable for the
                // lengths passed.
                let n = unsafe {
                    libc::recvfrom(
                        fd.as_raw_fd(),
                        buf.as_mut_ptr().cast(),
                        buf.len(),
                        0,
                        std::ptr::from_mut(&mut from).cast(),
                        &mut from_len,
                    )
                };
                let at = Instant::now();
                if n < 14 {
                    continue;
                }
                // Loopback shows every packet twice (sent, received).
                let outgoing = from.sll_pkttype == libc::PACKET_OUTGOING;
                if outgoing != loopback {
                    continue;
                }
                let frame = &buf[..n as usize];
                let (mut off, mut proto) = (14, u16::from_be_bytes([frame[12], frame[13]]));
                if proto == 0x8100 && frame.len() >= 18 {
                    off = 18;
                    proto = u16::from_be_bytes([frame[16], frame[17]]);
                }
                if !matches!(proto, 0x0800 | 0x86dd) {
                    continue;
                }
                let ip = &frame[off..];
                let dst_port_of = |ip: &[u8]| match ip.first()? >> 4 {
                    4 => {
                        let ihl = usize::from(ip[0] & 15) * 4;
                        (ip.len() >= ihl + 4 && ip[9] == 6)
                            .then(|| u16::from_be_bytes([ip[ihl + 2], ip[ihl + 3]]))
                    }
                    6 => {
                        (ip.len() >= 44 && ip[6] == 6).then(|| u16::from_be_bytes([ip[42], ip[43]]))
                    }
                    _ => None,
                };
                if dst_port_of(ip) == Some(dst_port) {
                    out.push(Raw {
                        at,
                        ip: ip.to_vec(),
                    });
                }
            }
            // Packets the kernel dropped because we were too slow.
            let mut stats = libc::tpacket_stats {
                tp_packets: 0,
                tp_drops: 0,
            };
            let mut len = std::mem::size_of::<libc::tpacket_stats>() as libc::socklen_t;
            // SAFETY: `stats` is live and writable for the length passed.
            let r = unsafe {
                libc::getsockopt(
                    fd.as_raw_fd(),
                    libc::SOL_PACKET,
                    libc::PACKET_STATISTICS,
                    std::ptr::from_mut(&mut stats).cast(),
                    &mut len,
                )
            };
            (out, if r == 0 { stats.tp_drops } else { 0 })
        });
        Ok(Self { stop, join })
    }

    /// Let in-flight packets arrive, stop, and return what was seen.
    pub fn finish(self) -> Vec<Raw> {
        let (raw, drops) = self.finish_stats();
        if drops > 0 {
            eprintln!("lb-fptest: the kernel dropped {drops} packets before capture saw them");
        }
        raw
    }

    /// Like [`Capture::finish`], also returning how many packets the kernel
    /// dropped because the capture fell behind. Zero means nothing was missed.
    pub fn finish_stats(self) -> (Vec<Raw>, u32) {
        std::thread::sleep(Duration::from_millis(300));
        self.stop.store(true, Ordering::Relaxed);
        self.join.join().unwrap_or_default()
    }
}

/// Captures client SYNs to one destination port on one interface.
pub struct SynCapture(Capture);

impl SynCapture {
    pub fn start(iface: &str, dst_port: u16) -> io::Result<Self> {
        Capture::start(iface, dst_port).map(Self)
    }

    /// Let in-flight packets arrive, stop, and return the SYNs seen (one per
    /// SYN on the wire, so retransmissions count).
    pub fn finish(self) -> Vec<Syn> {
        syns(&self.0.finish())
    }
}

/// What a captured flow says about the sender's stack, in the terms the
/// `profiles/tcp/` profiles use.
pub fn flow_summary(pkts: &[Pkt]) -> serde_json::Value {
    let syn_at: Vec<u64> = pkts
        .iter()
        .filter(|p| p.is_syn())
        .map(|p| p.at_ms)
        .collect();
    let first = syn_at.first().copied().unwrap_or(0);
    let ids: Vec<u16> = pkts.iter().map(|p| p.ip_id).collect();
    let steps = ids
        .windows(2)
        .filter(|w| w[1].wrapping_sub(w[0]) == 1)
        .count();
    let ip_id = if ids.is_empty() {
        "none"
    } else if ids.iter().all(|&i| i == 0) {
        "zero"
    } else if ids.len() > 1 && steps * 2 >= ids.len() - 1 {
        "sequential"
    } else {
        "random"
    };
    let ts: Vec<(u64, u32)> = pkts
        .iter()
        .filter_map(|p| p.timestamps.map(|t| (p.at_ms, t.0)))
        .collect();
    let ts_hz = match (ts.first(), ts.last()) {
        (Some(a), Some(b)) if b.0 > a.0 + 500 => {
            Some((f64::from(b.1.wrapping_sub(a.1)) * 1000.0 / (b.0 - a.0) as f64).round())
        }
        _ => None,
    };
    serde_json::json!({
        "packets": pkts.len(),
        "syn_times_ms": syn_at.iter().map(|t| t - first).collect::<Vec<_>>(),
        "syn_option_kinds": pkts.iter().filter(|p| p.is_syn()).map(|p| p.option_kinds.clone()).collect::<Vec<_>>(),
        "ip_id": ip_id,
        "ts_hz": ts_hz,
        "window_fields": pkts.iter().filter(|p| !p.is_syn()).map(|p| p.window).take(12).collect::<Vec<_>>(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ja4t_matches_the_published_shape() {
        // Windows: MSS, NOP, WS, NOP, NOP, SACK_PERM.
        let ip = windows_syn();
        let s = parse_syn(&ip).unwrap();
        assert_eq!(s.ja4t(), "64240_2-1-3-1-1-4_1460_8");
        assert_eq!((s.ttl, s.df, s.checksums_ok), (128, true, true));
    }

    #[test]
    fn a_corrupted_checksum_is_reported() {
        let mut ip = windows_syn();
        ip[20 + 16] ^= 0xff;
        assert!(!parse_syn(&ip).unwrap().checksums_ok);
    }

    /// Hand-assembled Windows SYN with correct checksums (computed here).
    fn windows_syn() -> Vec<u8> {
        let mut tcp = vec![0u8; 20];
        tcp[0..2].copy_from_slice(&50000u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&443u16.to_be_bytes());
        tcp[12] = 8 << 4;
        tcp[13] = 0x02;
        tcp[14..16].copy_from_slice(&64240u16.to_be_bytes());
        tcp.extend([2, 4, 5, 0xb4, 1, 3, 3, 8, 1, 1, 4, 2]);
        let (s, d) = ([10, 0, 0, 1], [10, 0, 0, 2]);
        let c = !folded(sum16(
            &tcp,
            sum16(&s, 0) + sum16(&d, 0) + 6 + tcp.len() as u32,
        ));
        tcp[16..18].copy_from_slice(&c.to_be_bytes());
        let mut ip = vec![0x45, 0];
        ip.extend(((20 + tcp.len()) as u16).to_be_bytes());
        ip.extend([0, 1, 0x40, 0, 128, 6, 0, 0]);
        ip.extend(s);
        ip.extend(d);
        let h = !folded(sum16(&ip, 0));
        ip[10..12].copy_from_slice(&h.to_be_bytes());
        ip.extend(tcp);
        ip
    }

    fn pkt(at_ms: u64, ip_id: u16, flags: u8, ts: Option<u32>) -> Pkt {
        Pkt {
            at_ms,
            src_port: 1,
            ttl: 64,
            ip_id,
            flags,
            window: 1000,
            seq: 0,
            payload_len: 0,
            option_kinds: vec![],
            window_scale: None,
            timestamps: ts.map(|t| (t, 0)),
        }
    }

    #[test]
    fn flow_summary_reads_the_stack_traits() {
        let seq: Vec<Pkt> = (0..6)
            .map(|i| {
                pkt(
                    i * 1000,
                    100 + i as u16,
                    if i < 3 { 2 } else { 0x10 },
                    Some(5000 + i as u32 * 1000),
                )
            })
            .collect();
        let s = flow_summary(&seq);
        assert_eq!(s["syn_times_ms"], serde_json::json!([0, 1000, 2000]));
        assert_eq!(s["ip_id"], "sequential");
        assert_eq!(s["ts_hz"], 1000.0);
        let zero: Vec<Pkt> = (0..4).map(|i| pkt(i * 10, 0, 0x10, None)).collect();
        assert_eq!(flow_summary(&zero)["ip_id"], "zero");
        let random: Vec<Pkt> = [5u16, 900, 12, 40000, 77]
            .iter()
            .enumerate()
            .map(|(i, &id)| pkt(i as u64, id, 0x10, None))
            .collect();
        assert_eq!(flow_summary(&random)["ip_id"], "random");
    }
}
