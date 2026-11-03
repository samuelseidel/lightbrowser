//! Rewrites an outgoing client SYN so its IP/TCP header matches a
//! [`TcpProfile`]. Pure functions on packet bytes: no sockets, no privileges.
//!
//! The rewrite only **removes, reorders or shrinks** what the kernel already
//! negotiated. It never adds a capability the kernel did not offer, so the
//! kernel's connection state stays consistent with what the peer was told:
//!
//! | Field | Rule |
//! |---|---|
//! | TTL / hop limit, DF, window | set to the profile value |
//! | Option order, NOP/EOL padding | the profile's |
//! | MSS | `min(kernel, profile)`: never above what the path carries |
//! | Window scale | `min(kernel, profile)`: a smaller shift makes the peer *under*-estimate our window, which is safe; a larger one would not be |
//! | SACK-permitted, timestamps | kept only if the kernel offered them; dropped if the profile lacks them. TSval/TSecr are the kernel's |
//! | ECE + CWR | set if `ecn` |
//!
//! Dropping timestamps is safe: with no TS in the SYN the peer sends none in
//! the SYN-ACK, and the kernel then runs the connection without them.
//!
//! Only IPv4 and IPv6 packets without extension headers are handled; anything
//! else is left alone.
//!
//! Beyond the SYN: [`classify`] names the connection a packet belongs to,
//! [`set_ipv4_id`] applies an IP ID policy and [`retransmit_syn`] builds the
//! copies of a SYN that a profile's retransmission schedule sends.

use std::net::IpAddr;

use super::profile::{TcpOpt, TcpProfile};

const PROTO_TCP: u8 = 6;
const FLAG_SYN: u8 = 0x02;
const FLAG_ACK: u8 = 0x10;
const FLAG_ECE: u8 = 0x40;
const FLAG_CWR: u8 = 0x80;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RewriteError {
    #[error("malformed packet: {0}")]
    Malformed(&'static str),
    #[error("profile options need {0} bytes, the limit is 40")]
    OptionsTooLong(usize),
}

/// Something the profile asked for that the rewrite refused to do because it
/// would desynchronise the kernel from the peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Limit {
    /// Profile window scale is larger than the kernel's; kept the kernel's.
    /// Fix with `TCP_WINDOW_CLAMP` (`TcpProfile::window_clamp`).
    WindowScaleRaised { kernel: u8, profile: u8 },
    /// Profile MSS is larger than the kernel's; kept the kernel's.
    MssRaised { kernel: u16, profile: u16 },
    /// Profile lists timestamps but the kernel did not offer them
    /// (`net.ipv4.tcp_timestamps=0`).
    TimestampsNotOffered,
    /// Profile lists SACK-permitted but the kernel did not offer it.
    SackNotOffered,
    /// Profile lists window scale but the kernel did not offer it.
    WindowScaleNotOffered,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Report {
    pub limits: Vec<Limit>,
    /// The kernel offered timestamps and the profile has none.
    pub dropped_timestamps: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// Not a client SYN (or not IPv4/IPv6 TCP). The packet is unchanged.
    Untouched,
    Rewritten(Report),
}

/// What the kernel offered in the SYN.
#[derive(Debug, Default)]
struct KernelOpts {
    mss: Option<u16>,
    ws: Option<u8>,
    sack_ok: bool,
    ts: Option<(u32, u32)>,
}

fn parse_options(mut b: &[u8]) -> Result<KernelOpts, RewriteError> {
    let mut o = KernelOpts::default();
    while let Some(&kind) = b.first() {
        match kind {
            0 => break,
            1 => b = &b[1..],
            _ => {
                let len = *b.get(1).ok_or(RewriteError::Malformed("option length"))? as usize;
                if len < 2 || len > b.len() {
                    return Err(RewriteError::Malformed("option length out of range"));
                }
                let body = &b[2..len];
                match (kind, body.len()) {
                    (2, 2) => o.mss = Some(u16::from_be_bytes([body[0], body[1]])),
                    (3, 1) => o.ws = Some(body[0]),
                    (4, 0) => o.sack_ok = true,
                    (8, 8) => {
                        o.ts = Some((
                            u32::from_be_bytes([body[0], body[1], body[2], body[3]]),
                            u32::from_be_bytes([body[4], body[5], body[6], body[7]]),
                        ));
                    }
                    _ => {}
                }
                b = &b[len..];
            }
        }
    }
    Ok(o)
}

/// The option block for `p`, built from what the kernel offered.
fn build_options(
    p: &TcpProfile,
    k: &KernelOpts,
    report: &mut Report,
) -> Result<Vec<u8>, RewriteError> {
    let mut out = Vec::with_capacity(40);
    for opt in &p.tcp.options {
        match opt {
            TcpOpt::Nop => out.push(1),
            TcpOpt::Eol => out.push(0),
            TcpOpt::Mss => {
                if let Some(km) = k.mss {
                    if p.tcp.mss > km {
                        report.limits.push(Limit::MssRaised {
                            kernel: km,
                            profile: p.tcp.mss,
                        });
                    }
                    out.extend([2, 4]);
                    out.extend(km.min(p.tcp.mss).to_be_bytes());
                }
            }
            TcpOpt::Ws => match k.ws {
                Some(kw) => {
                    if p.tcp.window_scale > kw {
                        report.limits.push(Limit::WindowScaleRaised {
                            kernel: kw,
                            profile: p.tcp.window_scale,
                        });
                    }
                    out.extend([3, 3, kw.min(p.tcp.window_scale)]);
                }
                None => report.limits.push(Limit::WindowScaleNotOffered),
            },
            TcpOpt::SackOk => {
                if k.sack_ok {
                    out.extend([4, 2]);
                } else {
                    report.limits.push(Limit::SackNotOffered);
                }
            }
            TcpOpt::Ts => match k.ts {
                Some((val, ecr)) => {
                    out.extend([8, 10]);
                    out.extend(val.to_be_bytes());
                    out.extend(ecr.to_be_bytes());
                }
                None => report.limits.push(Limit::TimestampsNotOffered),
            },
        }
    }
    report.dropped_timestamps = k.ts.is_some() && !p.tcp.options.contains(&TcpOpt::Ts);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    if out.len() > 40 {
        return Err(RewriteError::OptionsTooLong(out.len()));
    }
    Ok(out)
}

/// Ones-complement sum of `data` as big-endian 16-bit words.
fn sum16(data: &[u8], mut acc: u32) -> u32 {
    let mut chunks = data.chunks_exact(2);
    for c in &mut chunks {
        acc += u32::from(u16::from_be_bytes([c[0], c[1]]));
    }
    if let [last] = chunks.remainder() {
        acc += u32::from(*last) << 8;
    }
    acc
}

fn fold(mut acc: u32) -> u16 {
    while acc > 0xffff {
        acc = (acc & 0xffff) + (acc >> 16);
    }
    !(acc as u16)
}

/// Internet checksum of `data` (checksum field zeroed by the caller).
pub fn checksum(data: &[u8]) -> u16 {
    fold(sum16(data, 0))
}

/// TCP checksum over `segment` with the pseudo header for `src`/`dst`.
pub fn tcp_checksum(src: &[u8], dst: &[u8], segment: &[u8]) -> u16 {
    let mut acc = sum16(src, 0);
    acc = sum16(dst, acc);
    if src.len() == 16 {
        // IPv6 pseudo header: 32-bit length, then 24 zero bits and next header.
        acc += (segment.len() as u32 >> 16) + (segment.len() as u32 & 0xffff);
    } else {
        acc += segment.len() as u32;
    }
    acc += u32::from(PROTO_TCP);
    fold(sum16(segment, acc))
}

/// Rewrite `pkt` in place if it is a client SYN. `pkt` starts at the IP
/// header (what NFQUEUE hands over).
pub fn rewrite_syn(pkt: &mut Vec<u8>, p: &TcpProfile) -> Result<Outcome, RewriteError> {
    match pkt.first().map(|b| b >> 4) {
        Some(4) => rewrite_v4(pkt, p),
        Some(6) => rewrite_v6(pkt, p),
        _ => Ok(Outcome::Untouched),
    }
}

/// A client SYN split into the fixed header, the options and the payload.
type SynParts<'a> = (&'a [u8], &'a [u8], &'a [u8]);

/// Splits a TCP segment into its parts if it is a client SYN.
fn split_syn(seg: &[u8]) -> Result<Option<SynParts<'_>>, RewriteError> {
    if seg.len() < 20 {
        return Err(RewriteError::Malformed("short tcp header"));
    }
    let flags = seg[13];
    if flags & (FLAG_SYN | FLAG_ACK | 0x04) != FLAG_SYN {
        return Ok(None);
    }
    let hlen = usize::from(seg[12] >> 4) * 4;
    if hlen < 20 || hlen > seg.len() {
        return Err(RewriteError::Malformed("tcp data offset"));
    }
    Ok(Some((&seg[..20], &seg[20..hlen], &seg[hlen..])))
}

/// Builds the new TCP segment (checksum still zero).
fn new_segment(
    p: &TcpProfile,
    seg: &[u8],
    report: &mut Report,
) -> Result<Option<Vec<u8>>, RewriteError> {
    let Some((head, old_opts, payload)) = split_syn(seg)? else {
        return Ok(None);
    };
    let kernel = parse_options(old_opts)?;
    let opts = build_options(p, &kernel, report)?;
    let mut out = Vec::with_capacity(20 + opts.len() + payload.len());
    out.extend_from_slice(head);
    out[12] = (((20 + opts.len()) / 4) as u8) << 4 | (out[12] & 0x0f);
    out[14..16].copy_from_slice(&p.tcp.window.to_be_bytes());
    out[16..18].copy_from_slice(&[0, 0]);
    if p.tcp.ecn {
        out[13] |= FLAG_ECE | FLAG_CWR;
    }
    out.extend_from_slice(&opts);
    out.extend_from_slice(payload);
    Ok(Some(out))
}

fn rewrite_v4(pkt: &mut Vec<u8>, p: &TcpProfile) -> Result<Outcome, RewriteError> {
    if pkt.len() < 20 {
        return Err(RewriteError::Malformed("short ipv4 header"));
    }
    let ihl = usize::from(pkt[0] & 0x0f) * 4;
    let total = usize::from(u16::from_be_bytes([pkt[2], pkt[3]]));
    if ihl < 20 || total < ihl || total > pkt.len() {
        return Err(RewriteError::Malformed("ipv4 lengths"));
    }
    let frag = u16::from_be_bytes([pkt[6], pkt[7]]);
    if pkt[9] != PROTO_TCP || frag & 0x1fff != 0 {
        return Ok(Outcome::Untouched);
    }
    let mut report = Report::default();
    let Some(seg) = new_segment(p, &pkt[ihl..total], &mut report)? else {
        return Ok(Outcome::Untouched);
    };
    let mut out = Vec::with_capacity(ihl + seg.len());
    out.extend_from_slice(&pkt[..ihl]);
    out[2..4].copy_from_slice(&((ihl + seg.len()) as u16).to_be_bytes());
    out[8] = p.ip.ttl;
    let flags_frag = if p.ip.df {
        frag | 0x4000
    } else {
        frag & !0x4000
    };
    out[6..8].copy_from_slice(&flags_frag.to_be_bytes());
    out[10..12].copy_from_slice(&[0, 0]);
    let hsum = checksum(&out[..ihl]);
    out[10..12].copy_from_slice(&hsum.to_be_bytes());
    let (src, dst) = (pkt[12..16].to_vec(), pkt[16..20].to_vec());
    out.extend_from_slice(&seg);
    let csum = tcp_checksum(&src, &dst, &out[ihl..]);
    out[ihl + 16..ihl + 18].copy_from_slice(&csum.to_be_bytes());
    *pkt = out;
    Ok(Outcome::Rewritten(report))
}

fn rewrite_v6(pkt: &mut Vec<u8>, p: &TcpProfile) -> Result<Outcome, RewriteError> {
    if pkt.len() < 40 {
        return Err(RewriteError::Malformed("short ipv6 header"));
    }
    let payload = usize::from(u16::from_be_bytes([pkt[4], pkt[5]]));
    if pkt[6] != PROTO_TCP {
        return Ok(Outcome::Untouched); // extension headers: not handled
    }
    if 40 + payload > pkt.len() {
        return Err(RewriteError::Malformed("ipv6 payload length"));
    }
    let mut report = Report::default();
    let Some(seg) = new_segment(p, &pkt[40..40 + payload], &mut report)? else {
        return Ok(Outcome::Untouched);
    };
    let mut out = Vec::with_capacity(40 + seg.len());
    out.extend_from_slice(&pkt[..40]);
    out[4..6].copy_from_slice(&(seg.len() as u16).to_be_bytes());
    out[7] = p.ip.ttl;
    let (src, dst) = (pkt[8..24].to_vec(), pkt[24..40].to_vec());
    out.extend_from_slice(&seg);
    let csum = tcp_checksum(&src, &dst, &out[40..]);
    out[56..58].copy_from_slice(&csum.to_be_bytes());
    *pkt = out;
    Ok(Outcome::Rewritten(report))
}

/// A connection, seen from the client.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct FlowKey {
    pub src: IpAddr,
    pub dst: IpAddr,
    pub sport: u16,
    pub dport: u16,
}

/// What a client TCP packet is, for connection tracking.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// SYN without ACK: a connection attempt or a retransmission of it.
    Syn,
    /// RST or FIN.
    Closing,
    /// Everything else; the first one after a SYN is the handshake ACK.
    Other,
}

/// The IP header length and total length of a TCP packet, or `None` if it is
/// not an unfragmented IPv4 or IPv6 (no extension headers) TCP packet.
fn tcp_layout(pkt: &[u8]) -> Option<(usize, usize)> {
    match pkt.first()? >> 4 {
        4 if pkt.len() >= 20 => {
            let ihl = usize::from(pkt[0] & 15) * 4;
            let total = usize::from(u16::from_be_bytes([pkt[2], pkt[3]]));
            let frag = u16::from_be_bytes([pkt[6], pkt[7]]);
            (ihl >= 20
                && total >= ihl + 20
                && total <= pkt.len()
                && pkt[9] == PROTO_TCP
                && frag & 0x1fff == 0)
                .then_some((ihl, total))
        }
        6 if pkt.len() >= 60 => {
            let total = 40 + usize::from(u16::from_be_bytes([pkt[4], pkt[5]]));
            (pkt[6] == PROTO_TCP && total >= 60 && total <= pkt.len()).then_some((40, total))
        }
        _ => None,
    }
}

/// Which connection `pkt` belongs to and what kind of packet it is.
pub fn classify(pkt: &[u8]) -> Option<(FlowKey, Kind)> {
    let (ip, total) = tcp_layout(pkt)?;
    let tcp = &pkt[ip..total];
    let addr = |b: &[u8]| -> IpAddr {
        if b.len() == 4 {
            <[u8; 4]>::try_from(b)
                .map(IpAddr::from)
                .unwrap_or([0, 0, 0, 0].into())
        } else {
            <[u8; 16]>::try_from(b)
                .map(IpAddr::from)
                .unwrap_or([0u8; 16].into())
        }
    };
    let (src, dst) = if ip == 40 {
        (addr(&pkt[8..24]), addr(&pkt[24..40]))
    } else {
        (addr(&pkt[12..16]), addr(&pkt[16..20]))
    };
    let flags = tcp[13];
    let kind = if flags & (FLAG_SYN | FLAG_ACK | 0x04) == FLAG_SYN {
        Kind::Syn
    } else if flags & 0x05 != 0 {
        Kind::Closing
    } else {
        Kind::Other
    };
    let key = FlowKey {
        src,
        dst,
        sport: u16::from_be_bytes([tcp[0], tcp[1]]),
        dport: u16::from_be_bytes([tcp[2], tcp[3]]),
    };
    Some((key, kind))
}

/// The TCP sequence number of `pkt`. A retransmitted SYN has the same one as
/// the original; a new connection that reuses the port has another.
pub fn tcp_seq(pkt: &[u8]) -> Option<u32> {
    let (ip, _) = tcp_layout(pkt)?;
    Some(u32::from_be_bytes(pkt[ip + 4..ip + 8].try_into().ok()?))
}

/// Recompute the IPv4 header checksum and the TCP checksum of `pkt`.
///
/// Needed after any edit, and even for the packets the kernel meant to
/// checksum in hardware: a packet handed back by the shim is treated as
/// complete, so a queued packet's partial TCP checksum would go out wrong.
pub fn refresh_checksums(pkt: &mut [u8]) {
    let Some((ip, total)) = tcp_layout(pkt) else {
        return;
    };
    if ip == 40 {
        let (src, dst) = (pkt[8..24].to_vec(), pkt[24..40].to_vec());
        pkt[56..58].copy_from_slice(&[0, 0]);
        let c = tcp_checksum(&src, &dst, &pkt[40..total]);
        pkt[56..58].copy_from_slice(&c.to_be_bytes());
        return;
    }
    pkt[10..12].copy_from_slice(&[0, 0]);
    let h = checksum(&pkt[..ip]);
    pkt[10..12].copy_from_slice(&h.to_be_bytes());
    let (src, dst) = (pkt[12..16].to_vec(), pkt[16..20].to_vec());
    pkt[ip + 16..ip + 18].copy_from_slice(&[0, 0]);
    let c = tcp_checksum(&src, &dst, &pkt[ip..total]);
    pkt[ip + 16..ip + 18].copy_from_slice(&c.to_be_bytes());
}

/// Set the IPv4 identification field and fix the checksums. Returns false
/// (and changes nothing) for IPv6 and for packets that are not TCP.
pub fn set_ipv4_id(pkt: &mut [u8], id: u16) -> bool {
    match tcp_layout(pkt) {
        Some((ip, _)) if ip != 40 => {
            pkt[4..6].copy_from_slice(&id.to_be_bytes());
            refresh_checksums(pkt);
            true
        }
        _ => false,
    }
}

/// Add `delta` ticks to the TSval of a SYN in the kernel's layout. Checksums
/// are not updated; [`rewrite_syn`] does that.
fn bump_tsval(pkt: &mut [u8], delta: u32) {
    let Some((ip, total)) = tcp_layout(pkt) else {
        return;
    };
    let hlen = usize::from(pkt[ip + 12] >> 4) * 4;
    let (mut i, end) = (ip + 20, (ip + hlen).min(total));
    while i < end {
        match pkt[i] {
            0 => break,
            1 => i += 1,
            k => {
                let Some(&len) = pkt.get(i + 1) else { break };
                let len = usize::from(len);
                if len < 2 || i + len > end {
                    break;
                }
                if k == 8 && len == 10 {
                    let v = u32::from_be_bytes([pkt[i + 2], pkt[i + 3], pkt[i + 4], pkt[i + 5]]);
                    pkt[i + 2..i + 6].copy_from_slice(&v.wrapping_add(delta).to_be_bytes());
                }
                i += len;
            }
        }
    }
}

/// The SYN that retransmission number `attempt` (1 = the first) sends, built
/// from the kernel's original SYN `kernel_syn` sent `elapsed_ms` earlier. It
/// has the profile's layout for that attempt (see `OptionVariant`) and the
/// timestamp the kernel itself would have sent by then.
pub fn retransmit_syn(
    kernel_syn: &[u8],
    p: &TcpProfile,
    attempt: u8,
    elapsed_ms: u32,
) -> Result<Vec<u8>, RewriteError> {
    let mut pkt = kernel_syn.to_vec();
    bump_tsval(&mut pkt, elapsed_ms);
    let mut profile = p.clone();
    if let Some(v) = p
        .syn_retransmit
        .iter()
        .flat_map(|r| &r.option_variants)
        .rfind(|v| v.from_attempt <= attempt)
    {
        profile.tcp.options.clone_from(&v.options);
    }
    match rewrite_syn(&mut pkt, &profile)? {
        Outcome::Rewritten(_) => Ok(pkt),
        Outcome::Untouched => Err(RewriteError::Malformed("not a client SYN")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn profile(name: &str) -> TcpProfile {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/tcp");
        TcpProfile::load(&dir.join(format!("{name}.json"))).unwrap()
    }

    const TSVAL: u32 = 0x1122_3344;

    /// A SYN as the Linux kernel writes it: MSS, SACK, TS, NOP, WS.
    fn linux_syn(v6: bool, kernel_ws: u8) -> Vec<u8> {
        let mut opts = vec![2, 4, 0x05, 0xb4, 4, 2, 8, 10];
        opts.extend(TSVAL.to_be_bytes());
        opts.extend([0, 0, 0, 0]);
        opts.extend([1, 3, 3, kernel_ws]);
        let mut tcp = vec![0u8; 20];
        tcp[0..2].copy_from_slice(&40000u16.to_be_bytes());
        tcp[2..4].copy_from_slice(&443u16.to_be_bytes());
        tcp[4..8].copy_from_slice(&0xdead_beefu32.to_be_bytes());
        tcp[12] = (((20 + opts.len()) / 4) as u8) << 4;
        tcp[13] = FLAG_SYN;
        tcp[14..16].copy_from_slice(&64240u16.to_be_bytes());
        tcp.extend(&opts);
        if v6 {
            let (src, dst) = (
                [0x20, 1, 0xdb, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1],
                [0x20, 1, 0xdb, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2],
            );
            let c = tcp_checksum(&src, &dst, &tcp);
            tcp[16..18].copy_from_slice(&c.to_be_bytes());
            let mut ip = vec![0x60, 0, 0, 0];
            ip.extend((tcp.len() as u16).to_be_bytes());
            ip.extend([PROTO_TCP, 64]);
            ip.extend(src);
            ip.extend(dst);
            ip.extend(tcp);
            ip
        } else {
            let (src, dst) = ([192, 0, 2, 1], [192, 0, 2, 2]);
            let c = tcp_checksum(&src, &dst, &tcp);
            tcp[16..18].copy_from_slice(&c.to_be_bytes());
            let mut ip = vec![0x45, 0];
            ip.extend(((20 + tcp.len()) as u16).to_be_bytes());
            ip.extend([0x1b, 0x2c, 0x40, 0x00, 64, PROTO_TCP, 0, 0]);
            ip.extend(src);
            ip.extend(dst);
            let h = checksum(&ip);
            ip[10..12].copy_from_slice(&h.to_be_bytes());
            ip.extend(tcp);
            ip
        }
    }

    /// Both checksums verify (sum including the field folds to zero).
    fn assert_valid(pkt: &[u8]) {
        let (ip, tcp, src, dst) = if pkt[0] >> 4 == 4 {
            let ihl = usize::from(pkt[0] & 15) * 4;
            assert_eq!(checksum(&pkt[..ihl]), 0, "ipv4 header checksum");
            assert_eq!(usize::from(u16::from_be_bytes([pkt[2], pkt[3]])), pkt.len());
            (ihl, &pkt[ihl..], &pkt[12..16], &pkt[16..20])
        } else {
            assert_eq!(
                usize::from(u16::from_be_bytes([pkt[4], pkt[5]])),
                pkt.len() - 40
            );
            (40, &pkt[40..], &pkt[8..24], &pkt[24..40])
        };
        let _ = ip;
        assert_eq!(tcp_checksum(src, dst, tcp), 0, "tcp checksum");
    }

    fn tcp_part(pkt: &[u8]) -> &[u8] {
        if pkt[0] >> 4 == 4 {
            &pkt[usize::from(pkt[0] & 15) * 4..]
        } else {
            &pkt[40..]
        }
    }

    fn opts_of(pkt: &[u8]) -> Vec<u8> {
        let t = tcp_part(pkt);
        t[20..usize::from(t[12] >> 4) * 4].to_vec()
    }

    #[test]
    fn windows_layout_over_ipv4() {
        let mut pkt = linux_syn(false, 10);
        let out = rewrite_syn(&mut pkt, &profile("windows_65535")).unwrap();
        assert_eq!(
            out,
            Outcome::Rewritten(Report {
                limits: vec![],
                dropped_timestamps: true
            })
        );
        assert_eq!(opts_of(&pkt), [2, 4, 5, 0xb4, 1, 3, 3, 8, 1, 1, 4, 2]);
        assert_eq!(pkt[8], 128, "ttl");
        assert_eq!(u16::from_be_bytes([pkt[6], pkt[7]]) & 0x4000, 0x4000, "df");
        assert_eq!(
            u16::from_be_bytes([tcp_part(&pkt)[14], tcp_part(&pkt)[15]]),
            65535
        );
        assert_valid(&pkt);
    }

    #[test]
    fn both_windows_variants_differ_only_in_the_window() {
        let (a, b) = (profile("windows_65535"), profile("windows_64240"));
        let (mut pa, mut pb) = (linux_syn(false, 10), linux_syn(false, 10));
        rewrite_syn(&mut pa, &a).unwrap();
        rewrite_syn(&mut pb, &b).unwrap();
        assert_eq!(opts_of(&pa), opts_of(&pb));
        assert_eq!(
            u16::from_be_bytes([tcp_part(&pa)[14], tcp_part(&pa)[15]]),
            65535
        );
        assert_eq!(
            u16::from_be_bytes([tcp_part(&pb)[14], tcp_part(&pb)[15]]),
            64240
        );
        assert_valid(&pa);
        assert_valid(&pb);
    }

    #[test]
    fn window_scale_is_lowered_never_raised() {
        // Kernel shift 10 -> profile 8 is safe and applied.
        let mut pkt = linux_syn(false, 10);
        rewrite_syn(&mut pkt, &profile("windows_65535")).unwrap();
        assert_eq!(opts_of(&pkt)[7], 8);
        // Kernel shift 7 < profile 8 would let the peer overrun us: kept at 7.
        let mut pkt = linux_syn(false, 7);
        let Outcome::Rewritten(r) = rewrite_syn(&mut pkt, &profile("windows_65535")).unwrap()
        else {
            panic!()
        };
        assert_eq!(opts_of(&pkt)[7], 7);
        assert_eq!(
            r.limits,
            [Limit::WindowScaleRaised {
                kernel: 7,
                profile: 8
            }]
        );
    }

    #[test]
    fn macos_layout_keeps_kernel_timestamps_and_pads_with_eol() {
        let mut pkt = linux_syn(false, 10);
        rewrite_syn(&mut pkt, &profile("macos_14")).unwrap();
        let o = opts_of(&pkt);
        let mut want = vec![2, 4, 5, 0xb4, 1, 3, 3, 6, 1, 1, 8, 10];
        want.extend(TSVAL.to_be_bytes());
        want.extend([0, 0, 0, 0, 4, 2, 0, 0]);
        assert_eq!(o, want);
        assert_eq!(o.len(), 24);
        assert_eq!(
            u16::from_be_bytes([tcp_part(&pkt)[14], tcp_part(&pkt)[15]]),
            65535
        );
        assert_eq!(pkt[8], 64);
        assert_eq!(
            tcp_part(&pkt)[13],
            FLAG_SYN | FLAG_ECE | FLAG_CWR,
            "macOS sets ECE+CWR on the SYN"
        );
        assert_valid(&pkt);
    }

    #[test]
    fn ipv6_gets_hop_limit_and_valid_checksum() {
        let mut pkt = linux_syn(true, 10);
        rewrite_syn(&mut pkt, &profile("windows_65535")).unwrap();
        assert_eq!(pkt[7], 128, "hop limit");
        assert_eq!(opts_of(&pkt), [2, 4, 5, 0xb4, 1, 3, 3, 8, 1, 1, 4, 2]);
        assert_valid(&pkt);
    }

    #[test]
    fn linux_profile_is_a_no_op_on_the_layout() {
        let mut pkt = linux_syn(false, 7);
        let before = opts_of(&pkt);
        rewrite_syn(&mut pkt, &profile("linux_6")).unwrap();
        assert_eq!(opts_of(&pkt), before);
        assert_valid(&pkt);
    }

    #[test]
    fn never_adds_timestamps_the_kernel_did_not_offer() {
        // Kernel SYN without TS: MSS, SACK, NOP, WS.
        let mut pkt = linux_syn(false, 7);
        let t = tcp_part(&pkt).to_vec();
        let ihl = 20;
        let mut seg = t[..20].to_vec();
        let opts = [2, 4, 5, 0xb4, 4, 2, 1, 3, 3, 7, 0, 0];
        seg[12] = (((20 + opts.len()) / 4) as u8) << 4;
        seg.extend(opts);
        pkt.truncate(ihl);
        pkt.extend(seg);
        let n = pkt.len() as u16;
        pkt[2..4].copy_from_slice(&n.to_be_bytes());
        let Outcome::Rewritten(r) = rewrite_syn(&mut pkt, &profile("macos_14")).unwrap() else {
            panic!()
        };
        assert_eq!(r.limits, [Limit::TimestampsNotOffered]);
        assert!(!opts_of(&pkt).windows(2).any(|w| w == [8, 10]));
        assert_valid(&pkt);
    }

    #[test]
    fn ecn_profile_sets_ece_and_cwr() {
        let mut p = profile("windows_65535");
        p.tcp.ecn = true;
        let mut pkt = linux_syn(false, 10);
        rewrite_syn(&mut pkt, &p).unwrap();
        assert_eq!(tcp_part(&pkt)[13], FLAG_SYN | FLAG_ECE | FLAG_CWR);
        assert_valid(&pkt);
    }

    #[test]
    fn non_syn_packets_are_left_alone() {
        for flags in [FLAG_SYN | FLAG_ACK, FLAG_ACK, 0x04] {
            let mut pkt = linux_syn(false, 10);
            pkt[20 + 13] = flags;
            let before = pkt.clone();
            assert_eq!(
                rewrite_syn(&mut pkt, &profile("windows_65535")).unwrap(),
                Outcome::Untouched
            );
            assert_eq!(pkt, before);
        }
        let mut udp = linux_syn(false, 10);
        udp[9] = 17;
        assert_eq!(
            rewrite_syn(&mut udp, &profile("windows_65535")).unwrap(),
            Outcome::Untouched
        );
    }

    #[test]
    fn malformed_packets_are_errors_not_panics() {
        for len in [0usize, 5, 19, 20, 39, 45] {
            let mut pkt = linux_syn(false, 10);
            pkt.truncate(len);
            let _ = rewrite_syn(&mut pkt, &profile("windows_65535"));
        }
        let mut pkt = linux_syn(false, 10);
        pkt[20 + 12] = 0xf0; // data offset past the end
        assert!(rewrite_syn(&mut pkt, &profile("windows_65535")).is_err());
        let mut pkt = linux_syn(false, 10);
        pkt[20 + 21] = 1; // option length 1 for MSS
        assert!(rewrite_syn(&mut pkt, &profile("windows_65535")).is_err());
    }

    #[test]
    fn classify_names_the_flow_and_the_kind() {
        let pkt = linux_syn(false, 10);
        let (k, kind) = classify(&pkt).unwrap();
        assert_eq!((k.sport, k.dport, kind), (40000, 443, Kind::Syn));
        assert_eq!(k.dst, "192.0.2.2".parse::<IpAddr>().unwrap());
        let (k6, _) = classify(&linux_syn(true, 10)).unwrap();
        assert!(k6.src.is_ipv6());
        for (flags, want) in [
            (FLAG_ACK, Kind::Other),
            (0x18, Kind::Other),
            (0x04, Kind::Closing),
            (0x11, Kind::Closing),
        ] {
            let mut p = linux_syn(false, 10);
            p[20 + 13] = flags;
            assert_eq!(classify(&p).unwrap().1, want, "{flags:#x}");
        }
        let mut udp = linux_syn(false, 10);
        udp[9] = 17;
        assert!(classify(&udp).is_none());
    }

    #[test]
    fn ip_id_edit_keeps_both_checksums_valid() {
        let mut pkt = linux_syn(false, 10);
        assert!(set_ipv4_id(&mut pkt, 0));
        assert_eq!(&pkt[4..6], [0, 0]);
        assert_valid(&pkt);
        // A queued packet carries only the pseudo-header seed in its TCP
        // checksum (hardware offload); the refresh must still make it whole.
        let mut offloaded = linux_syn(false, 10);
        offloaded[20 + 16..20 + 18].copy_from_slice(&[0x12, 0x34]);
        assert!(set_ipv4_id(&mut offloaded, 0xabcd));
        assert_valid(&offloaded);
        let mut v6 = linux_syn(true, 10);
        assert!(!set_ipv4_id(&mut v6, 1), "IPv6 has no id field");
    }

    #[test]
    fn retransmission_follows_the_option_variants_and_advances_the_clock() {
        let win = profile("windows_65535");
        let first = retransmit_syn(&linux_syn(false, 10), &win, 1, 1000).unwrap();
        assert_eq!(
            opts_of(&first),
            [2, 4, 5, 0xb4, 1, 3, 3, 8, 1, 1, 4, 2],
            "attempt 1 keeps window scale"
        );
        let late = retransmit_syn(&linux_syn(false, 10), &win, 2, 3000).unwrap();
        assert_eq!(
            opts_of(&late),
            [2, 4, 5, 0xb4, 1, 1, 4, 2],
            "attempt 2 drops it"
        );
        assert_valid(&late);

        let mac = retransmit_syn(&linux_syn(false, 10), &profile("macos_14"), 3, 3000).unwrap();
        let o = opts_of(&mac);
        let tsval = u32::from_be_bytes([o[12], o[13], o[14], o[15]]);
        assert_eq!(tsval, TSVAL + 3000, "TSval advances with the elapsed time");
        assert_valid(&mac);
    }
}
