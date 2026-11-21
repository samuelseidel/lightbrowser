//! A small `RTCPeerConnection` (chrome build): enough for the WebRTC
//! local-address probe without leaking anything. There is no network:
//! ICE gathering yields one host candidate with an mDNS name
//! (`<uuid>.local`), as Chrome does for pages without camera or
//! microphone permission, then ends. No STUN, no media, no data
//! transfer. The offer SDP has Chrome's layout for a data channel.
//!
//! Names (mDNS uuid, ICE ufrag/pwd, DTLS fingerprint) come from the
//! persona's audio seed and the page origin, so a persona gives the same
//! values every run.

use std::rc::Rc;

use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};

use super::navigator::NavObj;
use super::{Dict, K_OBJ, Obj, Runtime, TaskSource, Window, getter, method, this_obj};
use crate::registry::Registry;

/// RTC host objects (`Obj::Nav(NavObj::Rtc(..))`).
pub(crate) enum RtcObj {
    Peer(Box<Peer>),
    Channel(Box<Channel>),
    Candidate(Box<Candidate>),
    Description { kind: String, sdp: String },
}

impl RtcObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            RtcObj::Peer(_) => "RTCPeerConnection",
            RtcObj::Channel(_) => "RTCDataChannel",
            RtcObj::Candidate(_) => "RTCIceCandidate",
            RtcObj::Description { .. } => "RTCSessionDescription",
        }
    }
}

pub(crate) struct Peer {
    signaling: &'static str,
    gathering: &'static str,
    closed: bool,
    channels: u16,
    local: Option<(String, String)>,
    remote: Option<(String, String)>,
    ufrag: String,
    pwd: String,
    fingerprint: String,
    session: u64,
    host: String,
    port: u16,
    foundation: u32,
}

pub(crate) struct Channel {
    label: String,
    ordered: bool,
    protocol: String,
    negotiated: bool,
    id: Option<u16>,
    max_retransmits: Option<u16>,
    max_packet_life_time: Option<u16>,
    state: &'static str,
    binary_type: String,
}

pub(crate) struct Candidate {
    candidate: String,
    sdp_mid: Option<String>,
    sdp_mline_index: Option<u16>,
    ufrag: String,
    foundation: String,
    priority: Option<u32>,
    address: Option<String>,
    protocol: Option<String>,
    port: Option<u16>,
    kind: Option<String>,
}

fn rtc<R>(w: &Window, id: u32, f: impl FnOnce(&mut RtcObj) -> Option<R>) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Nav(n) => match &mut **n {
            NavObj::Rtc(r) => f(r),
            _ => None,
        },
        _ => None,
    })
    .flatten()
}

fn new_rtc(w: &Window, o: RtcObj) -> u32 {
    w.new_obj(Obj::Nav(Box::new(NavObj::Rtc(Box::new(o)))))
}

fn this_rtc<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&mut RtcObj) -> Option<R>,
) -> Result<R, HostError> {
    let id = this_obj(cx)?;
    rtc(w, id, f).ok_or_else(super::illegal)
}

fn peer<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&mut Peer) -> R,
) -> Result<R, HostError> {
    this_rtc(cx, w, |o| match o {
        RtcObj::Peer(p) => Some(f(p)),
        _ => None,
    })
}

/// A deterministic random stream for this persona, origin and purpose.
pub(crate) fn rng(w: &Window, what: &str) -> lb_persona::sfc32::Sfc32 {
    let mut key = w
        .profile
        .as_deref()
        .and_then(|p| lb_persona::profile::FingerprintSeeds::bytes(&p.fingerprint_seeds.audio_seed))
        .unwrap_or([0x5a; 16]);
    let origin = w.url().origin().ascii_serialization();
    for (i, b) in origin.bytes().chain(what.bytes()).enumerate() {
        key[(i * 7) % 16] = key[(i * 7) % 16].rotate_left(5) ^ b;
    }
    lb_persona::sfc32::Sfc32::new(key)
}

const ICE_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn ice_string(r: &mut lb_persona::sfc32::Sfc32, n: usize) -> String {
    (0..n)
        .map(|_| ICE_CHARS[(r.next_u32() % 64) as usize] as char)
        .collect()
}

fn new_peer(w: &Window, n: u32) -> Peer {
    let mut r = rng(w, &format!("pc{n}"));
    let ufrag = ice_string(&mut r, 4);
    let pwd = ice_string(&mut r, 24);
    let fingerprint = (0..32)
        .map(|_| format!("{:02X}", r.next_u32() & 0xff))
        .collect::<Vec<_>>()
        .join(":");
    let session = (u64::from(r.next_u32()) << 32 | u64::from(r.next_u32())) & 0x7fff_ffff_ffff_ffff;
    // One mDNS name per persona and origin, like Chrome's per-address name.
    let mut m = rng(w, "mdns");
    let b: Vec<u32> = (0..4).map(|_| m.next_u32()).collect();
    let host = format!(
        "{:08x}-{:04x}-4{:03x}-{:04x}-{:04x}{:08x}.local",
        b[0],
        b[1] >> 16,
        b[1] & 0xfff,
        (b[2] >> 16 & 0x3fff) | 0x8000,
        b[2] & 0xffff,
        b[3]
    );
    Peer {
        signaling: "stable",
        gathering: "new",
        closed: false,
        channels: 0,
        local: None,
        remote: None,
        ufrag,
        pwd,
        fingerprint,
        session: session.max(1_000_000_000_000_000_000),
        host,
        port: 49152 + (r.next_u32() % 16000) as u16,
        foundation: r.next_u32(),
    }
}

fn offer_sdp(p: &Peer) -> String {
    let mut s = format!(
        "v=0\r\no=- {} 2 IN IP4 127.0.0.1\r\ns=-\r\nt=0 0\r\n",
        p.session
    );
    if p.channels > 0 {
        s.push_str("a=group:BUNDLE 0\r\n");
    }
    s.push_str("a=extmap-allow-mixed\r\na=msid-semantic: WMS\r\n");
    if p.channels > 0 {
        s.push_str(&format!(
            "m=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\nc=IN IP4 0.0.0.0\r\n\
             a=ice-ufrag:{}\r\na=ice-pwd:{}\r\na=ice-options:trickle\r\n\
             a=fingerprint:sha-256 {}\r\na=setup:actpass\r\na=mid:0\r\n\
             a=sctp-port:5000\r\na=max-message-size:262144\r\n",
            p.ufrag, p.pwd, p.fingerprint
        ));
    }
    s
}

fn host_candidate(p: &Peer) -> Candidate {
    let priority = 2_113_937_151;
    Candidate {
        candidate: format!(
            "candidate:{} 1 udp {priority} {} {} typ host generation 0 ufrag {} network-cost 999",
            p.foundation, p.host, p.port, p.ufrag
        ),
        sdp_mid: Some("0".into()),
        sdp_mline_index: Some(0),
        ufrag: p.ufrag.clone(),
        foundation: p.foundation.to_string(),
        priority: Some(priority),
        address: Some(p.host.clone()),
        protocol: Some("udp".into()),
        port: Some(p.port),
        kind: Some("host".into()),
    }
}

/// Parse the fields of an `a=candidate` line (`RTCIceCandidate` ctor).
fn parse_candidate(
    line: &str,
    sdp_mid: Option<String>,
    idx: Option<u16>,
    ufrag: String,
) -> Candidate {
    let body = line.trim_start_matches("candidate:");
    let f: Vec<&str> = body.split_ascii_whitespace().collect();
    let get = |i: usize| f.get(i).map(|s| (*s).to_owned());
    let typ = f.iter().position(|x| *x == "typ").and_then(|i| get(i + 1));
    Candidate {
        candidate: line.to_owned(),
        sdp_mid,
        sdp_mline_index: idx,
        ufrag,
        foundation: get(0).unwrap_or_default(),
        priority: get(3).and_then(|s| s.parse().ok()),
        address: get(4),
        protocol: get(2).map(|s| s.to_ascii_lowercase()),
        port: get(5).and_then(|s| s.parse().ok()),
        kind: typ,
    }
}

fn desc_json(kind: &str, sdp: &str) -> String {
    format!(
        "{{\"type\":{},\"sdp\":{}}}",
        super::js_string_literal(kind),
        super::js_string_literal(sdp)
    )
}

/// Fire a plain event at a peer connection from a task.
fn fire(js: &mut dyn super::Js, w: &Rc<Window>, id: u32, type_: &str) {
    super::fire_event(js, w, HostRef::new(K_OBJ, id), type_, false, false);
}

/// ICE gathering after `setLocalDescription`: `gathering`, one host
/// candidate per data-channel offer, the end-of-candidates event,
/// `complete`.
fn start_gathering(w: &Window, id: u32) {
    w.queue_task(
        TaskSource::Networking,
        Box::new(move |js, w| {
            let Some((has_media, closed)) = rtc(w, id, |o| match o {
                RtcObj::Peer(p) => {
                    let go = !p.closed && p.gathering == "new";
                    if go {
                        p.gathering = "gathering";
                    }
                    Some((p.channels > 0, !go))
                }
                _ => None,
            }) else {
                return;
            };
            if closed {
                return;
            }
            fire(js, w, id, "icegatheringstatechange");
            if has_media
                && let Some(c) = rtc(w, id, |o| match o {
                    RtcObj::Peer(p) => Some(host_candidate(p)),
                    _ => None,
                })
            {
                let cand = new_rtc(w, RtcObj::Candidate(Box::new(c)));
                ice_event(js, w, id, Some(cand));
            }
            let done = rtc(w, id, |o| match o {
                RtcObj::Peer(p) if !p.closed => {
                    p.gathering = "complete";
                    Some(())
                }
                _ => None,
            });
            if done.is_some() {
                ice_event(js, w, id, None);
                fire(js, w, id, "icegatheringstatechange");
            }
        }),
    );
}

fn ice_event(js: &mut dyn super::Js, w: &Rc<Window>, pc: u32, cand: Option<u32>) {
    let ev = super::events::new_event(
        w,
        "RTCPeerConnectionIceEvent",
        "icecandidate",
        false,
        false,
        super::EventExtra::None,
    );
    w.nav_state.borrow_mut().ice_events.insert(ev, cand);
    super::events::dispatch(js, w, HostRef::new(K_OBJ, pc), ev);
}

fn opt_u16(cx: &mut CallCtx<'_, '_, '_>, d: &Dict, k: &str) -> Result<Option<u16>, HostError> {
    Ok(d.number(cx, k)?
        .filter(|n| n.is_finite())
        .map(|n| n.clamp(0.0, 65535.0) as u16))
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    super::ctor(reg, rt, "RTCPeerConnection", |cx, w| {
        if !cx.is_construct_call() {
            return Err(HostError::type_error(
                "Failed to construct 'RTCPeerConnection': Please use the 'new' operator, this DOM object constructor cannot be called as a function.",
            ));
        }
        if !cx.is_empty()
            && !matches!(
                cx.arg_kind(0),
                ValueKind::Object | ValueKind::Undefined | ValueKind::Null
            )
        {
            return Err(HostError::type_error(
                "Failed to construct 'RTCPeerConnection': The provided value is not of type 'RTCConfiguration'.",
            ));
        }
        let n = {
            let mut st = w.nav_state.borrow_mut();
            st.peers += 1;
            st.peers
        };
        let p = new_peer(w, n);
        let id = new_rtc(w, RtcObj::Peer(Box::new(p)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    macro_rules! pc_get {
        ($name:literal, |$p:ident| $e:expr) => {
            getter(reg, rt, "RTCPeerConnection", $name, |cx, w| {
                let v: Arg = peer(cx, w, |$p| Arg::from($e))?;
                cx.ret(v)
            });
        };
    }
    pc_get!("signalingState", |p| p.signaling);
    pc_get!("iceGatheringState", |p| p.gathering);
    pc_get!("iceConnectionState", |p| if p.closed {
        "closed"
    } else {
        "new"
    });
    pc_get!("connectionState", |p| if p.closed {
        "closed"
    } else {
        "new"
    });
    pc_get!("canTrickleIceCandidates", |p| if p.remote.is_some() {
        Arg::Bool(true)
    } else {
        Arg::Null
    });
    pc_get!("sctp", |_p| Arg::Null);
    for (name, local, which) in [
        ("localDescription", true, 0u8),
        ("currentLocalDescription", true, 1),
        ("pendingLocalDescription", true, 2),
        ("remoteDescription", false, 0),
        ("currentRemoteDescription", false, 1),
        ("pendingRemoteDescription", false, 2),
    ] {
        getter(reg, rt, "RTCPeerConnection", name, move |cx, w| {
            let d = peer(cx, w, |p| {
                let d = if local { &p.local } else { &p.remote };
                let stable = p.signaling == "stable";
                match which {
                    1 if !stable => None,
                    2 if stable => None,
                    _ => d.clone(),
                }
            })?;
            match d {
                Some((kind, sdp)) => {
                    let id = new_rtc(w, RtcObj::Description { kind, sdp });
                    cx.ret(w.obj_arg(id))
                }
                None => cx.ret(Arg::Null),
            }
        });
    }
    method(
        reg,
        rt,
        "RTCPeerConnection",
        "createDataChannel",
        |cx, w| {
            super::need_args(cx, 1, "createDataChannel", "RTCPeerConnection")?;
            let closed = peer(cx, w, |p| p.closed)?;
            if closed {
                return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'createDataChannel' on 'RTCPeerConnection': The RTCPeerConnection's signalingState is 'closed'.",
            ));
            }
            let label = cx.arg_string(0)?;
            let d = Dict::arg(cx, 1);
            let ordered = match d.get(cx, "ordered")? {
                Some(h) => {
                    let v = super::truthy(&cx.handle_value(h));
                    cx.release(h);
                    v
                }
                None => true,
            };
            let protocol = d.string(cx, "protocol")?.unwrap_or_default();
            let negotiated = d.bool(cx, "negotiated")?;
            let id = opt_u16(cx, &d, "id")?;
            let max_retransmits = opt_u16(cx, &d, "maxRetransmits")?;
            let max_packet_life_time = opt_u16(cx, &d, "maxPacketLifeTime")?;
            d.release(cx);
            peer(cx, w, |p| p.channels += 1)?;
            let ch = new_rtc(
                w,
                RtcObj::Channel(Box::new(Channel {
                    label,
                    ordered,
                    protocol,
                    negotiated,
                    id: if negotiated { id } else { None },
                    max_retransmits,
                    max_packet_life_time,
                    state: "connecting",
                    binary_type: "arraybuffer".into(),
                })),
            );
            cx.ret(w.obj_arg(ch))
        },
    );
    method(reg, rt, "RTCPeerConnection", "createOffer", |cx, w| {
        let r = cx.ret_new_promise()?;
        let sdp = peer(cx, w, |p| (!p.closed).then(|| offer_sdp(p)))?;
        match sdp {
            Some(sdp) => cx.resolve_promise(r, Arg::Json(desc_json("offer", &sdp))),
            None => cx.reject_promise(
                r,
                w.rt.err(
                    "InvalidStateError",
                    "Failed to execute 'createOffer' on 'RTCPeerConnection': The RTCPeerConnection's signalingState is 'closed'.",
                ),
            ),
        }
    });
    method(reg, rt, "RTCPeerConnection", "createAnswer", |cx, w| {
        let r = cx.ret_new_promise()?;
        peer(cx, w, |_| ())?;
        cx.reject_promise(
            r,
            w.rt.err(
                "InvalidStateError",
                "Failed to execute 'createAnswer' on 'RTCPeerConnection': PeerConnection cannot create an answer in a state other than have-remote-offer or have-local-pranswer.",
            ),
        )
    });
    method(
        reg,
        rt,
        "RTCPeerConnection",
        "setLocalDescription",
        |cx, w| {
            let id = this_obj(cx)?;
            let r = cx.ret_new_promise()?;
            let d = Dict::arg(cx, 0);
            let kind = d.string(cx, "type")?;
            let sdp = d.string(cx, "sdp")?;
            d.release(cx);
            let start = peer(cx, w, |p| {
                if p.closed {
                    return false;
                }
                let kind = kind.unwrap_or_else(|| "offer".into());
                let sdp = sdp
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| offer_sdp(p));
                p.signaling = if kind == "offer" {
                    "have-local-offer"
                } else {
                    "stable"
                };
                p.local = Some((kind, sdp));
                true
            })?;
            if start {
                w.queue_task(
                    TaskSource::Networking,
                    Box::new(move |js, w| fire(js, w, id, "signalingstatechange")),
                );
                start_gathering(w, id);
            }
            cx.resolve_promise(r, Arg::Undefined)
        },
    );
    method(
        reg,
        rt,
        "RTCPeerConnection",
        "setRemoteDescription",
        |cx, w| {
            let r = cx.ret_new_promise()?;
            let d = Dict::arg(cx, 0);
            let kind = d.string(cx, "type")?.unwrap_or_default();
            let sdp = d.string(cx, "sdp")?.unwrap_or_default();
            d.release(cx);
            peer(cx, w, |p| {
                p.signaling = if kind == "offer" {
                    "have-remote-offer"
                } else {
                    "stable"
                };
                p.remote = Some((kind, sdp));
            })?;
            cx.resolve_promise(r, Arg::Undefined)
        },
    );
    method(reg, rt, "RTCPeerConnection", "addIceCandidate", |cx, w| {
        let r = cx.ret_new_promise()?;
        peer(cx, w, |_| ())?;
        cx.resolve_promise(r, Arg::Undefined)
    });
    method(reg, rt, "RTCPeerConnection", "close", |cx, w| {
        peer(cx, w, |p| {
            p.closed = true;
            p.signaling = "closed";
        })?;
        Ok(())
    });
    method(reg, rt, "RTCPeerConnection", "restartIce", |cx, w| {
        peer(cx, w, |_| ())?;
        Ok(())
    });
    method(reg, rt, "RTCPeerConnection", "getConfiguration", |cx, w| {
        peer(cx, w, |_| ())?;
        cx.ret(Arg::Json(
            r#"{"iceServers":[],"iceTransportPolicy":"all","bundlePolicy":"balanced","rtcpMuxPolicy":"require","iceCandidatePoolSize":0}"#.into(),
        ))
    });
    for name in [
        "getSenders",
        "getReceivers",
        "getTransceivers",
        "getLocalStreams",
        "getRemoteStreams",
    ] {
        method(reg, rt, "RTCPeerConnection", name, |cx, w| {
            peer(cx, w, |_| ())?;
            cx.ret(Arg::Array(Vec::new()))
        });
    }

    // RTCDataChannel
    macro_rules! ch_get {
        ($name:literal, |$c:ident| $e:expr) => {
            getter(reg, rt, "RTCDataChannel", $name, |cx, w| {
                let v: Arg = this_rtc(cx, w, |o| match o {
                    RtcObj::Channel($c) => Some(Arg::from($e)),
                    _ => None,
                })?;
                cx.ret(v)
            });
        };
    }
    ch_get!("label", |c| c.label.clone());
    ch_get!("ordered", |c| c.ordered);
    ch_get!("protocol", |c| c.protocol.clone());
    ch_get!("negotiated", |c| c.negotiated);
    ch_get!("id", |c| c
        .id
        .map_or(Arg::Null, |v| Arg::from(u32::from(v))));
    ch_get!("maxRetransmits", |c| c
        .max_retransmits
        .map_or(Arg::Null, |v| Arg::from(u32::from(v))));
    ch_get!("maxPacketLifeTime", |c| c
        .max_packet_life_time
        .map_or(Arg::Null, |v| Arg::from(u32::from(v))));
    ch_get!("readyState", |c| c.state);
    ch_get!("bufferedAmount", |_c| 0);
    ch_get!("bufferedAmountLowThreshold", |_c| 0);
    ch_get!("binaryType", |c| c.binary_type.clone());
    ch_get!("reliable", |c| c.max_retransmits.is_none()
        && c.max_packet_life_time.is_none());
    method(reg, rt, "RTCDataChannel", "close", |cx, w| {
        this_rtc(cx, w, |o| match o {
            RtcObj::Channel(c) => {
                c.state = "closed";
                Some(())
            }
            _ => None,
        })
    });
    method(reg, rt, "RTCDataChannel", "send", |cx, w| {
        this_rtc(cx, w, |o| matches!(o, RtcObj::Channel(_)).then_some(()))?;
        Err(w.rt.err(
            "InvalidStateError",
            "Failed to execute 'send' on 'RTCDataChannel': RTCDataChannel.readyState is not 'open'",
        ))
    });

    // RTCIceCandidate
    super::ctor(reg, rt, "RTCIceCandidate", |cx, w| {
        let d = Dict::arg(cx, 0);
        let candidate = d.string(cx, "candidate")?.unwrap_or_default();
        let mid = d.string(cx, "sdpMid")?;
        let idx = d
            .number(cx, "sdpMLineIndex")?
            .filter(|n| n.is_finite())
            .map(|n| n as u16);
        let ufrag = d.string(cx, "usernameFragment")?.unwrap_or_default();
        d.release(cx);
        if mid.is_none() && idx.is_none() {
            return Err(HostError::type_error(
                "Failed to construct 'RTCIceCandidate': sdpMid and sdpMLineIndex are both null.",
            ));
        }
        let c = parse_candidate(&candidate, mid, idx, ufrag);
        let id = new_rtc(w, RtcObj::Candidate(Box::new(c)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    macro_rules! cand_get {
        ($name:literal, |$c:ident| $e:expr) => {
            getter(reg, rt, "RTCIceCandidate", $name, |cx, w| {
                let v: Arg = this_rtc(cx, w, |o| match o {
                    RtcObj::Candidate($c) => Some(Arg::from($e)),
                    _ => None,
                })?;
                cx.ret(v)
            });
        };
    }
    cand_get!("candidate", |c| c.candidate.clone());
    cand_get!("sdpMid", |c| c
        .sdp_mid
        .clone()
        .map_or(Arg::Null, Arg::String));
    cand_get!("sdpMLineIndex", |c| c
        .sdp_mline_index
        .map_or(Arg::Null, |v| Arg::from(u32::from(v))));
    cand_get!("foundation", |c| if c.foundation.is_empty() {
        Arg::Null
    } else {
        Arg::String(c.foundation.clone())
    });
    cand_get!("component", |c| if c.candidate.is_empty() {
        Arg::Null
    } else {
        Arg::from("rtp")
    });
    cand_get!("priority", |c| c.priority.map_or(Arg::Null, Arg::from));
    cand_get!("address", |c| c
        .address
        .clone()
        .map_or(Arg::Null, Arg::String));
    cand_get!("protocol", |c| c
        .protocol
        .clone()
        .map_or(Arg::Null, Arg::String));
    cand_get!("port", |c| c
        .port
        .map_or(Arg::Null, |p| Arg::from(u32::from(p))));
    cand_get!("type", |c| c.kind.clone().map_or(Arg::Null, Arg::String));
    cand_get!("tcpType", |_c| Arg::Null);
    cand_get!("relatedAddress", |_c| Arg::Null);
    cand_get!("relatedPort", |_c| Arg::Null);
    cand_get!("usernameFragment", |c| if c.ufrag.is_empty() {
        Arg::Null
    } else {
        Arg::String(c.ufrag.clone())
    });
    cand_get!("relayProtocol", |_c| Arg::Null);
    cand_get!("url", |_c| Arg::Null);
    method(reg, rt, "RTCIceCandidate", "toJSON", |cx, w| {
        let v = this_rtc(cx, w, |o| match o {
            RtcObj::Candidate(c) => {
                let opt =
                    |v: Option<String>| v.map_or("null".into(), |s| super::js_string_literal(&s));
                Some(format!(
                    "{{\"candidate\":{},\"sdpMid\":{},\"sdpMLineIndex\":{},\"usernameFragment\":{}}}",
                    super::js_string_literal(&c.candidate),
                    opt(c.sdp_mid.clone()),
                    c.sdp_mline_index.map_or("null".into(), |v| v.to_string()),
                    opt((!c.ufrag.is_empty()).then(|| c.ufrag.clone())),
                ))
            }
            _ => None,
        })?;
        cx.ret(Arg::Json(v))
    });

    // RTCSessionDescription
    super::ctor(reg, rt, "RTCSessionDescription", |cx, w| {
        let d = Dict::arg(cx, 0);
        let kind = d.string(cx, "type")?.unwrap_or_default();
        let sdp = d.string(cx, "sdp")?.unwrap_or_default();
        d.release(cx);
        let id = new_rtc(w, RtcObj::Description { kind, sdp });
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, "RTCSessionDescription", "type", |cx, w| {
        let v = this_rtc(cx, w, |o| match o {
            RtcObj::Description { kind, .. } => Some(kind.clone()),
            _ => None,
        })?;
        cx.ret(v)
    });
    getter(reg, rt, "RTCSessionDescription", "sdp", |cx, w| {
        let v = this_rtc(cx, w, |o| match o {
            RtcObj::Description { sdp, .. } => Some(sdp.clone()),
            _ => None,
        })?;
        cx.ret(v)
    });
    method(reg, rt, "RTCSessionDescription", "toJSON", |cx, w| {
        let v = this_rtc(cx, w, |o| match o {
            RtcObj::Description { kind, sdp } => Some(desc_json(kind, sdp)),
            _ => None,
        })?;
        cx.ret(Arg::Json(v))
    });

    // RTCPeerConnectionIceEvent.candidate
    getter(
        reg,
        rt,
        "RTCPeerConnectionIceEvent",
        "candidate",
        |cx, w| {
            let ev = this_obj(cx)?;
            let c = w.nav_state.borrow().ice_events.get(&ev).copied().flatten();
            match c {
                Some(id) => cx.ret(w.obj_arg(id)),
                None => cx.ret(Arg::Null),
            }
        },
    );
    getter(reg, rt, "RTCPeerConnectionIceEvent", "url", |cx, _w| {
        this_obj(cx)?;
        cx.ret(Arg::Null)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_candidate_lines() {
        let c = parse_candidate(
            "candidate:4238732206 1 udp 2113937151 5bc915b8-10be-4a64-91ff-bbf113b302cd.local 54597 typ host generation 0",
            Some("0".into()),
            Some(0),
            String::new(),
        );
        assert_eq!(c.foundation, "4238732206");
        assert_eq!(c.priority, Some(2_113_937_151));
        assert_eq!(c.port, Some(54597));
        assert_eq!(c.kind.as_deref(), Some("host"));
        assert_eq!(c.protocol.as_deref(), Some("udp"));
    }
}
