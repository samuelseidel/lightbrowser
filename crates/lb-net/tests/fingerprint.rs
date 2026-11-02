//! End-to-end network fingerprint check.
//!
//! Runs `lb_net::fetch` against `lb-fptest`'s capture server on 127.0.0.1 and
//! compares what went over the wire with the captured profiles in
//! `profiles/tls/`. Loopback only: no third-party hosts are contacted.
//!
//! - Chrome (`chrome_153_linux`, captured from Chromium 153): JA3 (over the
//!   canonical extension order, since Chrome permutes), JA4, JA4_r, every
//!   ClientHello list, the trust_anchors / server_padding payloads, the H2
//!   SETTINGS, WINDOW_UPDATE, pseudo-header order, HEADERS priority and the
//!   Akamai text must all match exactly.
//! - Firefox (`firefox_128_win10`, from stealth-browser): ciphers, groups,
//!   sigalgs, ALPN, cert compression and all H2 values must match. The TLS
//!   differences BoringSSL cannot close yet (ADR 0003) are pinned exactly and
//!   printed, so both a regression and a fix make the test fail.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::time::Duration;

use lb_fptest::clienthello::{ClientHello, is_grease};
use lb_fptest::fingerprint::{self, Ja4};
use lb_fptest::server::{Capture, CaptureServer, ServerConfig};
use lb_net::fetch::NetContext;
use lb_net::tls::TlsConnector;
use lb_net::{ClientHelloProfile, HttpVersion, Request};
use lb_persona::Persona;
use serde_json::Value;

fn load(id: &str) -> (Arc<ClientHelloProfile>, Value) {
    let path = format!(
        "{}/../../profiles/tls/{id}.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let json = std::fs::read_to_string(&path).expect("profile file");
    let profile = ClientHelloProfile::from_json(&json).expect("profile parses");
    (Arc::new(profile), serde_json::from_str(&json).unwrap())
}

/// `n` fetches (one connection each) of `https://localhost:PORT/`, with the
/// capture of each connection.
async fn fetch_captures(
    persona: Persona,
    profile: &Arc<ClientHelloProfile>,
    n: usize,
) -> Vec<Capture> {
    let (server, mut rx) = CaptureServer::bind("127.0.0.1:0", ServerConfig::default())
        .await
        .unwrap();
    let mut ctx = NetContext::new(Arc::new(persona), Arc::clone(profile)).unwrap();
    ctx.danger_accept_invalid_certs = true;
    // A host name, so SNI is sent as for a real site (none for IP literals).
    let url: url::Url = format!("https://localhost:{}/", server.local_addr().port())
        .parse()
        .unwrap();
    let mut caps = Vec::new();
    for _ in 0..n {
        let resp = lb_net::fetch(&mut ctx, Request::get(url.clone()))
            .await
            .unwrap();
        assert_eq!(resp.status, 200);
        assert_eq!(resp.version, HttpVersion::Http2);
        assert_eq!(&resp.body[..], b"lb-fptest ok\n");
        let cap = tokio::time::timeout(Duration::from_secs(5), rx.recv())
            .await
            .expect("capture in time")
            .expect("capture");
        assert!(cap.error.is_none(), "capture error: {:?}", cap.error);
        caps.push(cap);
    }
    caps
}

fn ids(entries: &[lb_net::profile::IanaEntry]) -> Vec<u16> {
    entries.iter().map(|e| e.id).collect()
}

fn no_grease(v: &[u16]) -> Vec<u16> {
    v.iter().copied().filter(|x| !is_grease(*x)).collect()
}

fn ext_set(ch: &ClientHello) -> BTreeSet<u16> {
    no_grease(&ch.extension_ids()).into_iter().collect()
}

/// The hello with its extensions in `order` (GREASE dropped), so a permuted
/// Chrome hello gets the profile's canonical JA3.
fn reordered(ch: &ClientHello, order: &[u16]) -> ClientHello {
    let mut c = ch.clone();
    c.extensions = order
        .iter()
        .filter_map(|id| ch.extensions.iter().find(|e| e.id == *id).cloned())
        .collect();
    c
}

/// Field `i` of a JA3 string as numbers.
fn ja3_field(ja3: &str, i: usize) -> Vec<u16> {
    let f = ja3.split(',').nth(i).unwrap();
    if f.is_empty() {
        return vec![];
    }
    f.split('-').map(|x| x.parse().unwrap()).collect()
}

fn settings(p: &ClientHelloProfile) -> Vec<(u16, u32)> {
    p.http2_settings
        .ordered_params
        .iter()
        .map(|s| (s.code().unwrap(), s.value))
        .collect()
}

#[tokio::test]
async fn chrome_matches_captured_chromium() {
    let (profile, json) = load("chrome_153_linux");
    assert!(
        TlsConnector::new(Arc::clone(&profile))
            .unwrap()
            .gaps()
            .is_empty()
    );
    let caps = fetch_captures(Persona::default_chrome(), &profile, 2).await;
    let order = ids(&profile.extensions_order);

    for cap in &caps {
        let ch = cap.client_hello.as_ref().unwrap();
        let ja4 = Ja4::from_client_hello(ch, 't');
        let canon = reordered(ch, &order);
        let h2 = cap.h2.as_ref().unwrap();
        println!("chrome measured:");
        println!("  ja3 (wire order)  {}", fingerprint::ja3_string(ch));
        println!("  ja3 (canonical)   {}", fingerprint::ja3_string(&canon));
        println!("  ja3 profile       {}", profile.ja3_string);
        println!("  ja4 {}  (profile {})", ja4.hashed(), profile.ja4);
        println!("  akamai {}", h2.akamai());

        // ClientHello lists.
        assert_eq!(cap.negotiated_alpn.as_deref(), Some("h2"));
        assert_eq!(ch.server_name.as_deref(), Some("localhost"));
        assert_eq!(no_grease(&ch.cipher_suites), ids(&profile.cipher_suites));
        assert_eq!(ext_set(ch), order.iter().copied().collect());
        assert_eq!(
            no_grease(&ch.supported_groups),
            ids(&profile.supported_groups)
        );
        assert_eq!(
            no_grease(&ch.signature_algorithms),
            ids(&profile.signature_algorithms)
        );
        assert_eq!(ch.alpn, profile.alpn_protocols);
        assert_eq!(ch.alps_protocols, profile.alps_protocols);
        assert_eq!(ch.compress_certificate, vec![2], "brotli");
        assert!(ch.uses_grease());
        assert!(is_grease(ch.cipher_suites[0]), "GREASE cipher first");

        // JA3 over the canonical order, JA4 and JA4_r exactly.
        assert_eq!(fingerprint::ja3_string(&canon), profile.ja3_string);
        assert_eq!(fingerprint::ja3(&canon), profile.ja3);
        assert_eq!(ja4.hashed(), profile.ja4);
        assert_eq!(ja4.raw(), json["ja4_r"].as_str().unwrap());

        // Payloads of the two newer extensions.
        let data = |id: u16| &ch.extensions.iter().find(|e| e.id == id).unwrap().data;
        assert_eq!(data(4832), &vec![0, 0], "server_padding request 0");
        let ta = data(51764);
        let ids_hex: String = ta[2..].iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(Some(ids_hex), profile.trust_anchor_ids);

        // HTTP/2 preface.
        assert_eq!(h2.settings, settings(&profile));
        assert_eq!(h2.window_update, Some(profile.http2_window_update));
        assert_eq!(h2.pseudo_header_order, profile.http2_pseudo_header_order);
        assert!(h2.priority_frames.is_empty());
        let want = profile.http2_priority.headers.unwrap();
        let got = h2.headers_priority.as_ref().expect("HEADERS priority");
        assert_eq!(
            (got.exclusive, got.depends_on, u16::from(got.weight) + 1),
            (want.exclusive, want.depends_on, want.weight)
        );
        assert_eq!(h2.frame_order, ["SETTINGS", "WINDOW_UPDATE", "HEADERS"]);
        assert_eq!(
            h2.akamai(),
            json["http2_akamai_fingerprint"].as_str().unwrap()
        );
    }

    // Chrome shuffles extensions per connection; so do we. Two equal orders
    // out of 15! would be a bug, not luck.
    let o1 = caps[0].client_hello.as_ref().unwrap().extension_ids();
    let o2 = caps[1].client_hello.as_ref().unwrap().extension_ids();
    assert_ne!(no_grease(&o1), no_grease(&o2), "extension order permuted");
}

#[tokio::test]
async fn firefox_matches_profile_where_boringssl_can() {
    let (profile, _) = load("firefox_128_win10");
    let connector = TlsConnector::new(Arc::clone(&profile)).unwrap();
    let caps = fetch_captures(Persona::default_firefox(), &profile, 1).await;
    let cap = &caps[0];
    let ch = cap.client_hello.as_ref().unwrap();
    let ja4 = Ja4::from_client_hello(ch, 't');
    let h2 = cap.h2.as_ref().unwrap();
    let sent = ext_set(ch);

    // Must match.
    assert_eq!(cap.negotiated_alpn.as_deref(), Some("h2"));
    assert!(!ch.uses_grease(), "Firefox sends no GREASE");
    assert_eq!(ch.cipher_suites, ids(&profile.cipher_suites));
    assert_eq!(ch.cipher_suites, ja3_field(&profile.ja3_string, 1));
    // JA4 part b: Firefox's known cipher hash.
    assert_eq!(ja4.hashed().split('_').nth(1), Some("5b57614c22b0"));
    assert_eq!(ch.signature_algorithms, ids(&profile.signature_algorithms));
    assert_eq!(ch.alpn, profile.alpn_protocols);
    assert_eq!(ch.compress_certificate, vec![2], "brotli");
    assert!(ch.alps_protocols.is_empty());
    assert_eq!(ch.ec_point_formats, vec![0]);
    let ffdhe = [256u16, 257];
    let groups: Vec<u16> = ids(&profile.supported_groups)
        .into_iter()
        .filter(|g| !ffdhe.contains(g))
        .collect();
    assert_eq!(ch.supported_groups, groups);

    assert_eq!(h2.settings, settings(&profile));
    assert_eq!(h2.window_update, Some(profile.http2_window_update));
    assert_eq!(h2.pseudo_header_order, profile.http2_pseudo_header_order);
    assert!(h2.priority_frames.is_empty());
    assert!(h2.headers_priority.is_none());
    assert_eq!(
        h2.akamai(),
        "1:65536;2:0;4:131072;5:16384|12517377|0|m,p,a,s"
    );

    // Documented differences (ADR 0003, tls.rs module docs). Pinned exactly.
    let listed: BTreeSet<u16> = ids(&profile.extensions_order).into_iter().collect();
    let ja3_exts: BTreeSet<u16> = ja3_field(&profile.ja3_string, 2).into_iter().collect();
    let missing: Vec<u16> = listed.difference(&sent).copied().collect();
    let extra: Vec<u16> = sent.difference(&listed).copied().collect();
    let not_in_ja3: Vec<u16> = sent.difference(&ja3_exts).copied().collect();
    let missing_from_ja3: Vec<u16> = ja3_exts.difference(&sent).copied().collect();
    println!("firefox expected differences (BoringSSL limits, see ADR 0003):");
    println!("  ja3 sent    {}", fingerprint::ja3_string(ch));
    println!("  ja3 profile {}", profile.ja3_string);
    println!("  ja4 sent    {}", ja4.hashed());
    println!("  extensions_order entries not sent: {missing:?}");
    println!("    21 padding (size-dependent), 28 record_size_limit,");
    println!("    34 delegated_credentials, 41 pre_shared_key (no resumption)");
    println!(
        "  vs profile ja3_string: sent but absent {not_in_ja3:?}, absent {missing_from_ja3:?}"
    );
    println!("    (the profile's ja3_string and extensions_order disagree on 18, 27, 65037)");
    println!(
        "  groups not sent: ffdhe2048, ffdhe3072 -> {:?}",
        connector.gaps()
    );
    println!(
        "  key_share sent {:?} (Firefox: [29, 23])",
        ch.key_share_groups
    );
    println!("  extension order: BoringSSL's fixed order, not Firefox's");
    assert_eq!(missing, [21, 28, 34, 41]);
    assert!(
        extra.is_empty(),
        "sent extensions not in the profile: {extra:?}"
    );
    assert_eq!(not_in_ja3, [18, 27]);
    assert_eq!(missing_from_ja3, [28, 34, 65037]);
    assert_eq!(
        connector.gaps(),
        [
            "supported_groups: ffdhe2048 (256)",
            "supported_groups: ffdhe3072 (257)"
        ]
    );
    assert_ne!(fingerprint::ja3_string(ch), profile.ja3_string);
}
