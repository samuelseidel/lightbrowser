//! WebSocket client (ADR 0025) against loopback servers only.
//!
//! - `wss:` handshake against `lb-fptest`'s capture server: the ClientHello
//!   (JA4: ALPN `http/1.1` alone, no ALPS, no `server_padding`) and the
//!   handshake request's header names in order must equal what Chrome 153
//!   (Chrome for Testing 153.0.8010.12) sent for `new WebSocket('wss://...')`
//!   on loopback (`lb-fptest serve`, 2026-11-04).
//! - `ws:` against a small RFC 6455 server here: masked client frames,
//!   `permessage-deflate` both ways, a server ping, the closing handshake.

use std::io::Read;
use std::sync::Arc;
use std::time::Duration;

use flate2::read::DeflateDecoder;
use lb_fptest::fingerprint;
use lb_fptest::server::{CaptureServer, ServerConfig};
use lb_net::ClientHelloProfile;
use lb_net::fetch::NetContext;
use lb_net::websocket::{WsCommand, WsConnect, WsEvent};
use lb_persona::Persona;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// Chrome 153's `wss:` ClientHello and handshake header names, recorded.
const CHROME_JA4: &str = "t13d1515h1_8daaf6152771_cab8d0f75fe8";
const CHROME_HEADERS: &[&str] = &[
    "Host",
    "Connection",
    "Pragma",
    "Cache-Control",
    "User-Agent",
    "Upgrade",
    "Origin",
    "Sec-WebSocket-Version",
    "Accept-Encoding",
    "Accept-Language",
    "Sec-WebSocket-Key",
    "Sec-WebSocket-Extensions",
];

fn ctx() -> NetContext {
    let path = format!(
        "{}/../../profiles/tls/chrome_153_linux.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let profile = ClientHelloProfile::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    let mut c = NetContext::new(Arc::new(Persona::default_chrome()), Arc::new(profile)).unwrap();
    c.danger_accept_invalid_certs = true;
    c
}

#[tokio::test]
async fn wss_hello_and_handshake_match_chrome() {
    let (server, mut rx) = CaptureServer::bind(
        "127.0.0.1:0",
        ServerConfig {
            alpn: vec![b"http/1.1".to_vec()],
            ..ServerConfig::default()
        },
    )
    .await
    .unwrap();
    let mut c = ctx();
    let url = format!("wss://localhost:{}/ws", server.local_addr().port())
        .parse()
        .unwrap();
    let (_tx, mut events) = c.websocket(WsConnect {
        url,
        protocols: vec![],
        origin: "http://127.0.0.1:18448".into(),
    });
    let cap = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("capture in time")
        .expect("capture");
    let ch = cap.client_hello.expect("client hello");
    assert_eq!(fingerprint::ja4(&ch), CHROME_JA4);
    assert_eq!(cap.negotiated_alpn.as_deref(), Some("http/1.1"));
    let h1 = cap.h1.expect("request");
    assert_eq!(h1.request_line, "GET /ws HTTP/1.1");
    let names: Vec<&str> = h1.headers.iter().map(|(k, _)| k.as_str()).collect();
    assert_eq!(names, CHROME_HEADERS);
    let get = |n: &str| {
        h1.headers
            .iter()
            .find(|(k, _)| k == n)
            .map(|(_, v)| v.clone())
    };
    assert_eq!(get("Origin").as_deref(), Some("http://127.0.0.1:18448"));
    assert_eq!(
        get("Sec-WebSocket-Extensions").as_deref(),
        Some("permessage-deflate; client_max_window_bits")
    );
    assert_eq!(
        get("User-Agent"),
        Some(Persona::default_chrome().user_agent.clone())
    );
    // The capture server answers 200, not 101: the connection fails.
    let mut saw_failed = false;
    while let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(5), events.recv()).await {
        match ev {
            WsEvent::Failed(m) => {
                assert!(m.contains("Unexpected response code"), "{m}");
                saw_failed = true;
            }
            WsEvent::Closed { code, clean, .. } => {
                assert_eq!((code, clean), (1006, false));
                break;
            }
            other => panic!("{other:?}"),
        }
    }
    assert!(saw_failed);
}

fn accept_key(key: &str) -> String {
    let d = boring::hash::hash(
        boring::hash::MessageDigest::sha1(),
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    )
    .unwrap();
    boring::base64::encode_block(&d)
}

/// One frame from the client: (fin, rsv1, opcode, unmasked payload).
async fn read_frame(s: &mut tokio::net::TcpStream) -> (bool, bool, u8, Vec<u8>) {
    let mut h = [0u8; 2];
    s.read_exact(&mut h).await.unwrap();
    assert!(h[1] & 0x80 != 0, "client frames are masked");
    let mut len = u64::from(h[1] & 0x7f);
    if len == 126 {
        let mut b = [0u8; 2];
        s.read_exact(&mut b).await.unwrap();
        len = u64::from(u16::from_be_bytes(b));
    } else if len == 127 {
        let mut b = [0u8; 8];
        s.read_exact(&mut b).await.unwrap();
        len = u64::from_be_bytes(b);
    }
    let mut mask = [0u8; 4];
    s.read_exact(&mut mask).await.unwrap();
    let mut p = vec![0u8; len as usize];
    s.read_exact(&mut p).await.unwrap();
    for (i, b) in p.iter_mut().enumerate() {
        *b ^= mask[i % 4];
    }
    (h[0] & 0x80 != 0, h[0] & 0x40 != 0, h[0] & 0x0f, p)
}

async fn next(events: &mut tokio::sync::mpsc::UnboundedReceiver<WsEvent>) -> WsEvent {
    tokio::time::timeout(Duration::from_secs(5), events.recv())
        .await
        .expect("event in time")
        .expect("event")
}

fn inflate(p: &[u8]) -> Vec<u8> {
    let mut data = p.to_vec();
    data.extend_from_slice(&[0, 0, 0xff, 0xff]);
    let mut out = Vec::new();
    let mut d = DeflateDecoder::new(&data[..]);
    let _ = d.read_to_end(&mut out);
    out
}

#[tokio::test]
async fn ws_frames_deflate_ping_and_close() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut head = Vec::new();
        while !head.ends_with(b"\r\n\r\n") {
            let mut b = [0u8; 1];
            s.read_exact(&mut b).await.unwrap();
            head.push(b[0]);
        }
        let head = String::from_utf8(head).unwrap();
        let key = head
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
            .unwrap()
            .trim()
            .to_owned();
        let resp = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\nSec-WebSocket-Accept: {}\r\nSec-WebSocket-Protocol: chat\r\nSec-WebSocket-Extensions: permessage-deflate\r\n\r\n",
            accept_key(&key)
        );
        s.write_all(resp.as_bytes()).await.unwrap();
        // A compressed text frame from the client.
        let (fin, rsv1, op, p) = read_frame(&mut s).await;
        assert!(fin && rsv1);
        assert_eq!(op, 1);
        assert_eq!(inflate(&p), b"hello hello hello");
        // Binary, also compressed.
        let (_, rsv1, op, p) = read_frame(&mut s).await;
        assert!(rsv1);
        assert_eq!(op, 2);
        assert_eq!(inflate(&p), vec![7u8; 300]);
        // Ping: the client answers with a pong carrying the same data.
        s.write_all(&[0x89, 2, b'h', b'i']).await.unwrap();
        let (_, _, op, p) = read_frame(&mut s).await;
        assert_eq!((op, p.as_slice()), (0xA, &b"hi"[..]));
        // An uncompressed text message, then one in two fragments.
        s.write_all(&[0x81, 3, b'a', b'b', b'c']).await.unwrap();
        s.write_all(&[0x01, 2, b'd', b'e']).await.unwrap();
        s.write_all(&[0x80, 1, b'f']).await.unwrap();
        // The client's close (code 4000, reason "bye"); answer it.
        let (_, _, op, p) = read_frame(&mut s).await;
        assert_eq!(op, 8);
        assert_eq!(&p[..2], &4000u16.to_be_bytes());
        assert_eq!(&p[2..], b"bye");
        s.write_all(&[0x88, 2, 0x0f, 0xa0]).await.unwrap();
        drop(s);
    });
    let mut c = ctx();
    let (tx, mut events) = c.websocket(WsConnect {
        url: format!("ws://127.0.0.1:{port}/chat").parse().unwrap(),
        protocols: vec!["chat".into()],
        origin: "http://127.0.0.1".into(),
    });
    assert_eq!(
        next(&mut events).await,
        WsEvent::Open {
            protocol: "chat".into(),
            extensions: "permessage-deflate".into()
        }
    );
    tx.send(WsCommand::Text("hello hello hello".into()))
        .unwrap();
    assert_eq!(next(&mut events).await, WsEvent::Flushed(17));
    tx.send(WsCommand::Binary(vec![7u8; 300])).unwrap();
    assert_eq!(next(&mut events).await, WsEvent::Flushed(300));
    assert_eq!(next(&mut events).await, WsEvent::Text("abc".into()));
    assert_eq!(next(&mut events).await, WsEvent::Text("def".into()));
    tx.send(WsCommand::Close(Some(4000), "bye".into())).unwrap();
    assert_eq!(
        next(&mut events).await,
        WsEvent::Closed {
            code: 4000,
            reason: String::new(),
            clean: true
        }
    );
    server.await.unwrap();
}
