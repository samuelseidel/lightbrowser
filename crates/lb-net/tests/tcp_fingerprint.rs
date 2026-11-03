//! TCP/IP SYN fingerprint test (ADR 0007). Loopback only.
//!
//! For each profile in `profiles/tcp/`, `lb_net::fetch` runs through a
//! `NetContext` against a local HTTP server. `lb-fptest` captures the real
//! SYN on `lo` with its own parser and the test compares it with the profile:
//! TTL, DF, window, MSS, window scale, option bytes and order, JA4T, and
//! valid checksums. The response body is large, so a connection whose state
//! disagrees with what the peer was told would stall or fail here.
//!
//! Needs root (`CAP_NET_RAW` to capture, `CAP_NET_ADMIN` for the shim, and
//! `iptables`). Without them the test prints why and skips. Run with
//! `--nocapture` to see the measured values.

#![cfg(target_os = "linux")]

use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;

use lb_fptest::tcp::{Capture, SynCapture, packets, syns};
use lb_net::fetch::{NetContext, Request};
use lb_net::tcpfp::nfq::{RuleGuard, Shim, ShimHandle};
use lb_net::tcpfp::profile::IdPolicy;
use lb_net::tcpfp::{TcpOpt, TcpProfile};
use lb_net::{ClientHelloProfile, fetch};
use lb_persona::Persona;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

const BODY_LEN: usize = 3 << 20;

/// Tests that install or depend on the iptables rule must not overlap.
static RULES: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn profiles() -> Vec<TcpProfile> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/tcp");
    TcpProfile::load_dir(&dir).unwrap()
}

fn tls_profile(persona: &Persona) -> Arc<ClientHelloProfile> {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/tls");
    Arc::new(
        ClientHelloProfile::load(dir.join(format!("{}.json", persona.tls_profile_id))).unwrap(),
    )
}

/// One-shot HTTP/1.1 server with a large body.
async fn serve() -> SocketAddr {
    let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = l.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut s, _)) = l.accept().await {
            tokio::spawn(async move {
                let mut buf = [0u8; 4096];
                let _ = s.read(&mut buf).await;
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {BODY_LEN}\r\nConnection: close\r\n\r\n"
                );
                let _ = s.write_all(head.as_bytes()).await;
                let _ = s.write_all(&vec![b'x'; BODY_LEN]).await;
            });
        }
    });
    addr
}

/// A persona whose User-Agent names the profile's operating system.
fn persona_for(p: &TcpProfile) -> Persona {
    let mut persona = Persona::default_chrome();
    persona.user_agent = match p.os.as_str() {
        "windows" => persona.user_agent,
        "macos" => "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36".into(),
        _ => "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/153.0.0.0 Safari/537.36".into(),
    };
    persona.tcp_profile_id = p.profile_id.clone();
    persona
}

fn can_capture() -> Result<(), String> {
    SynCapture::start("lo", 1)
        .map(|c| drop(c.finish()))
        .map_err(|e| format!("cannot capture on lo: {e}"))
}

#[tokio::test(flavor = "multi_thread")]
async fn syn_matches_each_profile() {
    let _lock = RULES.lock().await;
    if let Err(why) = can_capture() {
        eprintln!("SKIP tcp fingerprint test: {why}");
        return;
    }
    let all = profiles();
    let shim_ok = std::process::Command::new("iptables")
        .arg("-V")
        .output()
        .is_ok_and(|o| o.status.success());
    let addr = serve().await;

    for (i, p) in all.iter().enumerate() {
        // Windows and macOS need the shim; Linux must work without it.
        let mut shim = None;
        let mut _rules = None;
        if p.needs_shim() {
            if !shim_ok {
                eprintln!("SKIP {}: no iptables", p.profile_id);
                continue;
            }
            let queue = 4200 + i as u16;
            let s = match Shim::bind(queue, all.clone()) {
                Ok(s) => s,
                Err(e) => {
                    eprintln!(
                        "SKIP {}: cannot bind the netfilter queue (needs CAP_NET_ADMIN): {e}",
                        p.profile_id
                    );
                    continue;
                }
            };
            _rules = Some(RuleGuard::install(queue).expect("install iptables rule"));
            shim = Some(s.spawn());
        }

        let persona = Arc::new(persona_for(p));
        let mut ctx = NetContext::new(Arc::clone(&persona), tls_profile(&persona))
            .unwrap()
            .with_tcp_profile(Arc::new(p.clone()))
            .unwrap();
        let cap = SynCapture::start("lo", addr.port()).unwrap();
        let url = url::Url::parse(&format!("http://{addr}/")).unwrap();
        let resp = fetch(&mut ctx, Request::get(url))
            .await
            .unwrap_or_else(|e| panic!("{}: fetch failed: {e}", p.profile_id));
        let syns = cap.finish();
        let stats = shim.map(|h| h.stop().unwrap());

        assert_eq!(resp.body.len(), BODY_LEN, "{}: body", p.profile_id);
        assert_eq!(
            syns.len(),
            1,
            "{}: expected one SYN, saw {syns:?}",
            p.profile_id
        );
        let s = &syns[0];
        println!(
            "{:14} {}  ttl={} df={} win={} mss={:?} ws={:?} opts={:02x?}  shim={stats:?}",
            p.profile_id,
            s.ja4t(),
            s.ttl,
            s.df,
            s.window,
            s.mss,
            s.window_scale,
            s.option_bytes
        );

        // Only rewritten packets carry a full checksum on the sending host
        // (the kernel offloads the rest), so only they can be verified here.
        if p.needs_shim() {
            assert!(s.checksums_ok, "{}: bad checksum on the wire", p.profile_id);
        }
        assert_eq!(s.ttl, p.ip.ttl, "{}: ttl", p.profile_id);
        assert_eq!(s.df, p.ip.df, "{}: df", p.profile_id);
        assert_eq!(s.window, p.tcp.window, "{}: window", p.profile_id);
        assert_eq!(
            s.flags & 0xc0 == 0xc0,
            p.tcp.ecn,
            "{}: ECE+CWR on the SYN",
            p.profile_id
        );
        assert_eq!(s.mss, Some(p.tcp.mss), "{}: mss", p.profile_id);
        assert_eq!(
            s.window_scale,
            Some(p.tcp.window_scale),
            "{}: window scale",
            p.profile_id
        );
        let kinds: Vec<u8> = p.tcp.options.iter().map(|o| o.kind()).collect();
        assert_eq!(s.option_kinds, kinds, "{}: option order", p.profile_id);
        assert_eq!(
            s.timestamps.is_some(),
            p.tcp.options.contains(&TcpOpt::Ts),
            "{}: timestamps",
            p.profile_id
        );
        assert_eq!(s.ja4t(), p.expected.ja4t, "{}: JA4T", p.profile_id);
        if let Some(st) = stats {
            assert!(
                st.rewritten >= 1,
                "{}: shim did not rewrite: {st:?}",
                p.profile_id
            );
            assert_eq!(
                st.limited, 0,
                "{}: rewrite hit a kernel-state limit: {st:?}",
                p.profile_id
            );
            assert_eq!(
                st.dropped, 0,
                "{}: shim dropped packets: {st:?}",
                p.profile_id
            );
        }
    }
}

/// With no profile the host's own stack is on the wire (control for the test
/// above: proves the capture can tell the difference).
#[tokio::test(flavor = "multi_thread")]
async fn without_a_profile_the_host_stack_shows() {
    if let Err(why) = can_capture() {
        eprintln!("SKIP: {why}");
        return;
    }
    let addr = serve().await;
    let persona = Arc::new(Persona::default_chrome());
    let mut ctx = NetContext::new(Arc::clone(&persona), tls_profile(&persona)).unwrap();
    let cap = SynCapture::start("lo", addr.port()).unwrap();
    let url = url::Url::parse(&format!("http://{addr}/")).unwrap();
    fetch(&mut ctx, Request::get(url)).await.unwrap();
    let syns = cap.finish();
    assert_eq!(syns.len(), 1);
    println!("host stack: {}", syns[0].ja4t());
    assert_eq!(
        syns[0].option_kinds,
        [2, 4, 8, 1, 3],
        "the kernel's own layout"
    );
}

/// A persona must not claim one OS and send another's SYN.
#[test]
fn tcp_profile_must_match_the_user_agent() {
    let all = profiles();
    let mac = all.iter().find(|p| p.os == "macos").unwrap();
    let persona = Arc::new(Persona::default_chrome()); // Windows UA
    let ctx = NetContext::new(Arc::clone(&persona), tls_profile(&persona)).unwrap();
    let err = ctx
        .with_tcp_profile(Arc::new(mac.clone()))
        .err()
        .expect("mismatch must fail");
    assert!(err.to_string().contains("windows"), "{err}");
}

/// If the shim is not running, marked SYNs must be dropped, never sent with
/// the host's own stack (the rule has no `--queue-bypass`).
#[tokio::test(flavor = "multi_thread")]
async fn shim_down_fails_closed() {
    let _lock = RULES.lock().await;
    if let Err(why) = can_capture() {
        eprintln!("SKIP: {why}");
        return;
    }
    let all = profiles();
    let win = all.iter().find(|p| p.os == "windows").unwrap();
    let Ok(rules) = RuleGuard::install(4299) else {
        eprintln!("SKIP: cannot install the iptables rule");
        return;
    };
    let addr = serve().await;
    let cap = SynCapture::start("lo", addr.port()).unwrap();
    let r = tokio::time::timeout(
        std::time::Duration::from_millis(1500),
        lb_net::tcpfp::connect(addr, Some(win)),
    )
    .await;
    let syns = cap.finish();
    drop(rules);
    assert!(
        r.is_err(),
        "connect must not succeed while the shim is down: {r:?}"
    );
    assert!(
        syns.is_empty(),
        "no SYN may reach the wire while the shim is down: {syns:?}"
    );
}

/// With no rule installed a shim profile is refused up front, so the host's
/// own SYN cannot leak by accident.
#[tokio::test(flavor = "multi_thread")]
async fn shim_profile_needs_the_rule() {
    let _lock = RULES.lock().await;
    if can_capture().is_err() || lb_net::tcpfp::nfq::rule_installed().is_err() {
        eprintln!("SKIP: needs root and iptables");
        return;
    }
    let all = profiles();
    let win = all.iter().find(|p| p.os == "windows").unwrap();
    let persona = Arc::new(Persona::default_chrome());
    let ctx = NetContext::new(Arc::clone(&persona), tls_profile(&persona)).unwrap();
    let err = ctx
        .with_tcp_profile(Arc::new(win.clone()))
        .err()
        .expect("no rule: must fail");
    assert!(err.to_string().contains("not installed"), "{err}");

    let _rules = RuleGuard::install(4298).unwrap();
    let ctx = NetContext::new(Arc::clone(&persona), tls_profile(&persona)).unwrap();
    ctx.with_tcp_profile(Arc::new(win.clone()))
        .expect("rule present: must be accepted");
}

/// Bring up the shim and its iptables rules for `p` if it needs them.
fn shim_for(p: &TcpProfile, all: &[TcpProfile], queue: u16) -> Option<(ShimHandle, RuleGuard)> {
    if !p.needs_shim() {
        return None;
    }
    let shim = Shim::bind(queue, all.to_vec()).expect("bind the netfilter queue");
    let rules = RuleGuard::install(queue).expect("install iptables rules");
    Some((shim.spawn(), rules))
}

fn context_for(p: &TcpProfile) -> NetContext {
    let persona = Arc::new(persona_for(p));
    NetContext::new(Arc::clone(&persona), tls_profile(&persona))
        .unwrap()
        .with_tcp_profile(Arc::new(p.clone()))
        .unwrap()
}

fn get(addr: SocketAddr) -> Request {
    Request::get(url::Url::parse(&format!("http://{addr}/")).unwrap())
}

/// The IP ID of every packet follows the profile: macOS sends 0, Windows one
/// counter across connections, Linux the kernel's per-connection counter.
#[tokio::test(flavor = "multi_thread")]
async fn ip_id_follows_the_profile() {
    let _lock = RULES.lock().await;
    if let Err(why) = can_capture() {
        eprintln!("SKIP: {why}");
        return;
    }
    let all = profiles();
    let addr = serve().await;
    for (i, p) in all.iter().enumerate() {
        let shim = shim_for(p, &all, 4300 + i as u16);
        let mut ctx = context_for(p);
        let cap = Capture::start("lo", addr.port()).unwrap();
        for _ in 0..2 {
            let r = fetch(&mut ctx, get(addr)).await.unwrap();
            assert_eq!(r.body.len(), BODY_LEN, "{}: body", p.profile_id);
        }
        let (raw, lost) = cap.finish_stats();
        assert_eq!(lost, 0, "{}: the capture lost packets", p.profile_id);
        let pkts = packets(&raw);
        let stats = shim.map(|(h, _rules)| h.stop().unwrap());
        let ids: Vec<u16> = pkts.iter().map(|k| k.ip_id).collect();
        println!(
            "{:14} {} packets, ids {:?}…  {stats:?}",
            p.profile_id,
            ids.len(),
            &ids[..ids.len().min(6)]
        );
        assert!(
            ids.len() >= 6,
            "{}: expected two connections, saw {ids:?}",
            p.profile_id
        );
        match p.ip.id {
            IdPolicy::Zero => assert!(ids.iter().all(|&x| x == 0), "{}: {ids:?}", p.profile_id),
            IdPolicy::HostCounter => {
                // One counter for both connections, +1 per packet, in order.
                assert!(
                    ids.windows(2).all(|w| w[1] == w[0].wrapping_add(1)),
                    "{}: not one sequential counter: {ids:?}",
                    p.profile_id
                );
            }
            IdPolicy::Kernel => {
                // Left alone: each connection counts +1 per packet on its own
                // (a Linux per-socket counter), and none is 0.
                let mut ports: Vec<u16> = pkts.iter().map(|k| k.src_port).collect();
                ports.sort_unstable();
                ports.dedup();
                assert_eq!(ports.len(), 2, "{}: two connections", p.profile_id);
                for port in ports {
                    let own: Vec<u16> = pkts
                        .iter()
                        .filter(|k| k.src_port == port)
                        .map(|k| k.ip_id)
                        .collect();
                    assert!(own.iter().all(|&x| x != 0), "{own:?}");
                    assert!(
                        own.windows(2).all(|w| w[1] == w[0].wrapping_add(1)),
                        "{own:?}"
                    );
                }
            }
        }
        if let Some(st) = stats {
            assert!(
                st.id_rewritten as usize >= ids.len(),
                "{}: {st:?}",
                p.profile_id
            );
            assert_eq!(st.dropped, 0, "{}: {st:?}", p.profile_id);
        }
    }
}

/// An iptables rule that makes the local server ignore SYNs to one port, so
/// the client has to retransmit. Removed on drop.
struct IgnoreSyns(u16, bool);

impl IgnoreSyns {
    fn args(&self, action: &str) -> Vec<String> {
        [
            "-t",
            "filter",
            action,
            "INPUT",
            "-p",
            "tcp",
            "--syn",
            "--dport",
            &self.0.to_string(),
            "-j",
            "DROP",
        ]
        .map(String::from)
        .to_vec()
    }
    fn on(port: u16) -> Self {
        let g = Self(port, true);
        let ok = std::process::Command::new("iptables")
            .args(g.args("-I"))
            .status()
            .unwrap()
            .success();
        assert!(ok, "cannot install the DROP rule");
        g
    }
    fn off(&mut self) {
        if std::mem::take(&mut self.1) {
            let _ = std::process::Command::new("iptables")
                .args(self.args("-D"))
                .status();
        }
    }
}

impl Drop for IgnoreSyns {
    fn drop(&mut self) {
        self.off();
    }
}

/// An unanswered SYN is retransmitted on the profile's schedule, with its
/// option variants and an advancing timestamp, instead of the kernel's.
#[tokio::test(flavor = "multi_thread")]
async fn syn_retransmission_follows_the_schedule() {
    let _lock = RULES.lock().await;
    if let Err(why) = can_capture() {
        eprintln!("SKIP: {why}");
        return;
    }
    let all = profiles();
    let addr = serve().await;
    // (profile os, how long the server ignores SYNs, expected SYN times in ms)
    for (os, ignore_ms, want) in [
        ("macos", 2500u64, vec![0u64, 1000, 2000, 3000]),
        ("windows", 3500, vec![0, 1000, 3000, 7000]),
    ] {
        let (i, p) = all.iter().enumerate().find(|(_, p)| p.os == os).unwrap();
        let (shim, rules) = shim_for(p, &all, 4320 + i as u16).unwrap();
        let mut ctx = context_for(p);
        let mut ignore = IgnoreSyns::on(addr.port());
        let cap = Capture::start("lo", addr.port()).unwrap();
        let task =
            tokio::spawn(async move { fetch(&mut ctx, get(addr)).await.map(|r| r.body.len()) });
        tokio::time::sleep(std::time::Duration::from_millis(ignore_ms)).await;
        ignore.off();
        let len = task
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{os}: fetch failed: {e}"));
        let raw = cap.finish();
        let stats = shim.stop().unwrap();
        drop(rules);

        let seen: Vec<_> = syns(&raw)
            .into_iter()
            .zip(packets(&raw).into_iter().filter(|k| k.is_syn()))
            .collect();
        let times: Vec<u64> = seen.iter().map(|(_, k)| k.at_ms).collect();
        println!(
            "{os:8} SYNs at {times:?} ms; kinds {:?}; {stats:?}",
            seen.iter()
                .map(|(s, _)| s.option_kinds.clone())
                .collect::<Vec<_>>()
        );
        assert_eq!(
            len, BODY_LEN,
            "{os}: the connection works after the retransmissions"
        );
        assert_eq!(
            times.len(),
            want.len(),
            "{os}: SYN count (kernel retransmits must not leak): {times:?}"
        );
        for (got, want) in times.iter().zip(&want) {
            assert!(
                got.abs_diff(*want) <= 150,
                "{os}: SYN times {times:?}, want {want:?}"
            );
        }
        assert!(stats.kernel_retransmits_dropped >= 1, "{os}: {stats:?}");
        assert_eq!(
            stats.retransmits_sent as usize,
            want.len() - 1,
            "{os}: {stats:?}"
        );
        assert_eq!(stats.dropped, 0, "{os}: {stats:?}");

        let first = &seen[0].0;
        for (s, k) in &seen {
            assert_eq!(s.ttl, p.ip.ttl);
            assert_eq!(s.window, p.tcp.window);
            assert_eq!(s.mss, Some(p.tcp.mss));
            // The timestamp keeps the kernel's 1 kHz clock between SYNs.
            if let (Some((t0, _)), Some((t, _))) = (first.timestamps, s.timestamps) {
                assert!(
                    u64::from(t.wrapping_sub(t0)).abs_diff(k.at_ms) <= 100,
                    "{os}: TSval drift"
                );
            }
        }
        if os == "windows" {
            // Window scaling is dropped from the 2nd retransmission (attempt 2).
            let kinds: Vec<_> = seen.iter().map(|(s, _)| s.option_kinds.clone()).collect();
            assert_eq!(kinds[0], [2, 1, 3, 1, 1, 4]);
            assert_eq!(kinds[1], [2, 1, 3, 1, 1, 4]);
            assert_eq!(kinds[2], [2, 1, 1, 4]);
            assert_eq!(kinds[3], [2, 1, 1, 4]);
        } else {
            assert!(
                seen.iter()
                    .all(|(s, _)| s.timestamps.is_some()
                        && s.window_scale == Some(p.tcp.window_scale))
            );
        }
    }
}
