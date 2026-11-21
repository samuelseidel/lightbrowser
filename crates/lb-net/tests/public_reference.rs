//! Public data is the source of truth for the TCP profiles (ADR 0033).
//!
//! `profiles/reference/tcp_public.json` holds shares computed from real client
//! SYNs in a public dataset (`scripts/tcp_public_reference.py`). Every trait a
//! profile asserts must be seen in real traffic for its operating system, at
//! least [`MIN_SHARE`] of sessions. The test needs no privileges.
//!
//! The "Linux" class is mostly Android in that data (labels come from the
//! User-Agent), so for Linux only the layout, TTL, ECN and IP ID are checked;
//! its window and scale are measured on the kernel instead.

use std::path::Path;

use lb_net::tcpfp::profile::IdPolicy;
use lb_net::tcpfp::{TcpOpt, TcpProfile};
use lb_persona::Persona;
use serde_json::Value;

/// A trait must appear in at least this share of the sessions of its OS.
const MIN_SHARE: f64 = 0.05;

fn root() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles")
}

fn reference() -> Value {
    let text = std::fs::read_to_string(root().join("reference/tcp_public.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

fn profiles() -> Vec<TcpProfile> {
    TcpProfile::load_dir(&root().join("tcp")).unwrap()
}

/// The option layout in the dataset's notation: M mss, N nop, W window scale,
/// S sack-permitted, T timestamps, E end-of-list or padding byte.
fn letters(options: &[TcpOpt]) -> String {
    let (mut out, mut bytes) = (String::new(), 0);
    for o in options {
        out.push(match o {
            TcpOpt::Mss => 'M',
            TcpOpt::Nop => 'N',
            TcpOpt::Ws => 'W',
            TcpOpt::SackOk => 'S',
            TcpOpt::Ts => 'T',
            TcpOpt::Eol => 'E',
        });
        bytes += o.len();
    }
    while bytes % 4 != 0 {
        out.push('E');
        bytes += 1;
    }
    out
}

fn share(map: &Value, key: &str) -> f64 {
    map.get(key).and_then(Value::as_f64).unwrap_or(0.0)
}

#[test]
fn every_profile_trait_is_seen_in_public_traffic() {
    let reference = reference();
    for p in profiles() {
        let class = &reference["classes"][p.os.as_str()];
        assert!(
            class["sessions"].as_u64().unwrap_or(0) > 1000,
            "{}: no data for {}",
            p.profile_id,
            p.os
        );
        let ecn = class["ecn_on_syn"].as_f64().unwrap();
        let zero = class["ip_id_zero"].as_f64().unwrap();
        let mut checks = vec![
            (
                "option layout",
                share(&class["option_order"], &letters(&p.tcp.options)),
            ),
            ("ttl", share(&class["ttl"], &p.ip.ttl.to_string())),
            ("ecn on the SYN", if p.tcp.ecn { ecn } else { 1.0 - ecn }),
            (
                "ip id",
                if p.ip.id == IdPolicy::Zero {
                    zero
                } else {
                    1.0 - zero
                },
            ),
        ];
        if p.os != "linux" {
            checks.push(("window", share(&class["window"], &p.tcp.window.to_string())));
            checks.push((
                "window scale",
                share(&class["window_scale"], &p.tcp.window_scale.to_string()),
            ));
        }
        for (name, s) in checks {
            println!(
                "{:14} {:14} seen in {:5.1}% of {} sessions",
                p.profile_id,
                name,
                s * 100.0,
                p.os
            );
            assert!(
                s >= MIN_SHARE,
                "{}: {name} is seen in only {:.1}% of real {} sessions (need {:.0}%); \
                 see profiles/reference/tcp_public.json",
                p.profile_id,
                s * 100.0,
                p.os,
                MIN_SHARE * 100.0
            );
        }
    }
}

/// The default Windows persona should send what most real Windows clients send.
#[test]
fn default_persona_uses_the_most_common_windows_window() {
    let reference = reference();
    let windows = reference["classes"]["windows"]["window"]
        .as_object()
        .unwrap();
    let most_common: u16 = windows
        .iter()
        .max_by(|a, b| a.1.as_f64().partial_cmp(&b.1.as_f64()).unwrap())
        .unwrap()
        .0
        .parse()
        .unwrap();
    let id = Persona::default_chrome().tcp_profile_id;
    let p = profiles().into_iter().find(|p| p.profile_id == id).unwrap();
    assert_eq!(p.tcp.window, most_common, "default persona profile {id}");
}

/// Profiles that claim public data must say what is not covered by it.
#[test]
fn provenance_is_honest() {
    for p in profiles() {
        let v = &p.provenance;
        assert!(
            ["captured", "public-data", "documented-unverified"].contains(&v.status.as_str()),
            "{}: status {:?}",
            p.profile_id,
            v.status
        );
        if p.syn_retransmit.is_some() {
            assert!(
                v.unverified.iter().any(|u| u == "syn_retransmit"),
                "{}: no public dataset covers SYN retransmit timing; list it as unverified",
                p.profile_id
            );
        }
        assert!(
            !v.notes.is_empty(),
            "{}: notes name the sources",
            p.profile_id
        );
    }
}

#[test]
fn the_reference_records_where_it_came_from() {
    let r = reference();
    let src = &r["source"];
    assert!(src["url"].as_str().unwrap().starts_with("https://"));
    assert!(src["entries"].as_u64().unwrap() > 1000);
    assert!(src["licence_note"].as_str().unwrap().contains("not copied"));
    assert!(
        src["regenerate"]
            .as_str()
            .unwrap()
            .contains("tcp_public_reference.py")
    );
}
