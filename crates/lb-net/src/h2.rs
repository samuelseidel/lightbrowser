//! HTTP/2 over an established TLS stream.
//!
//! The connection preface (SETTINGS order and values, the initial
//! WINDOW_UPDATE) and the pseudo-header order come from the profile.
//! Upstream `h2` hard-codes both, so this module drives the vendored fork in
//! `vendor/h2` (see `vendor/h2/LIGHTBROWSER.md`).
//!
//! Request headers go out in the order given. `http::HeaderMap` iterates in
//! insertion order for distinct names; repeated names are grouped at the
//! position of their first occurrence. Response headers come back the same
//! way: arrival order, with repeated names grouped.

use bytes::{Bytes, BytesMut};
use h2::client::{Builder, PseudoHeader};
use http::{HeaderMap, HeaderName, HeaderValue, Uri, Version};

use crate::HttpVersion;
use crate::error::NetError;
use crate::fetch::{RawResponse, Request};
use crate::headers::OrderedHeaders;
use crate::profile::ClientHelloProfile;

/// Default cap on the (still encoded) response body: 32 MiB.
pub const DEFAULT_MAX_BODY: usize = 32 << 20;

/// HTTP/2 default connection window (RFC 9113 §6.9.2).
const DEFAULT_CONN_WINDOW: u32 = 65_535;

/// Headers HTTP/2 forbids (RFC 9113 §8.2.2). `host` is replaced by
/// `:authority`. `te` is dropped unless it is `trailers`.
const CONNECTION_SPECIFIC: &[&str] = &[
    "connection",
    "host",
    "keep-alive",
    "proxy-connection",
    "transfer-encoding",
    "upgrade",
];

/// Send `req` with a body cap of [`DEFAULT_MAX_BODY`].
pub async fn send<S>(
    io: S,
    req: &Request,
    profile: &ClientHelloProfile,
) -> Result<RawResponse, NetError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    send_with_limit(io, req, profile, DEFAULT_MAX_BODY).await
}

/// Open an HTTP/2 connection over `io` shaped by `profile`, send `req` and
/// read the response. Fails with [`NetError::BodyTooLarge`] once the body
/// exceeds `max_body` bytes. The connection is closed when the response has
/// been read.
pub async fn send_with_limit<S>(
    io: S,
    req: &Request,
    profile: &ClientHelloProfile,
    max_body: usize,
) -> Result<RawResponse, NetError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let builder = builder_for(profile)?;
    let request = build_request(req)?;

    let (client, conn) = builder.handshake::<_, Bytes>(io).await?;
    // The driver writes the preface and frames. It ends when `client` and all
    // streams are dropped (h2 then sends GOAWAY) or the peer closes.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!("h2 connection ended: {e}");
        }
    });

    let mut client = client.ready().await?;
    let end_of_stream = req.body.is_empty();
    let (response, mut stream) = client.send_request(request, end_of_stream)?;
    if !end_of_stream {
        send_body(&mut stream, req.body.clone()).await?;
    }

    let response = response.await?;
    let (parts, mut body) = response.into_parts();
    drop(client);

    let mut buf = BytesMut::new();
    while let Some(chunk) = body.data().await {
        let chunk = chunk?;
        let _ = body.flow_control().release_capacity(chunk.len());
        if buf.len() + chunk.len() > max_body {
            return Err(NetError::BodyTooLarge { limit: max_body });
        }
        buf.extend_from_slice(&chunk);
    }

    Ok(RawResponse {
        status: parts.status,
        headers: ordered(&parts.headers),
        body: buf.freeze(),
        version: HttpVersion::Http2,
    })
}

/// Send `body` on `stream`, waiting for flow-control capacity.
async fn send_body(stream: &mut h2::SendStream<Bytes>, mut body: Bytes) -> Result<(), NetError> {
    while !body.is_empty() {
        stream.reserve_capacity(body.len());
        let cap = match std::future::poll_fn(|cx| stream.poll_capacity(cx)).await {
            Some(cap) => cap?,
            None => return Err(NetError::Http("h2 stream closed while sending body".into())),
        };
        if cap == 0 {
            continue;
        }
        let chunk = body.split_to(cap.min(body.len()));
        stream.send_data(chunk, body.is_empty())?;
    }
    Ok(())
}

/// An h2 client builder whose preface matches `profile`.
pub fn builder_for(profile: &ClientHelloProfile) -> Result<Builder, NetError> {
    let mut b = Builder::new();
    let mut order = Vec::new();
    for s in &profile.http2_settings.ordered_params {
        let v = s.value;
        match s.id.as_str() {
            "HEADER_TABLE_SIZE" => {
                b.header_table_size(v);
            }
            "ENABLE_PUSH" => {
                b.enable_push(v != 0);
            }
            "MAX_CONCURRENT_STREAMS" => {
                b.max_concurrent_streams(v);
            }
            "INITIAL_WINDOW_SIZE" => {
                if v > (1 << 31) - 1 {
                    return Err(NetError::Profile(format!(
                        "INITIAL_WINDOW_SIZE {v} too large"
                    )));
                }
                b.initial_window_size(v);
            }
            "MAX_FRAME_SIZE" => {
                // The builder panics outside this range (RFC 9113 §6.5.2).
                if !(16_384..=16_777_215).contains(&v) {
                    return Err(NetError::Profile(format!(
                        "MAX_FRAME_SIZE {v} out of range"
                    )));
                }
                b.max_frame_size(v);
            }
            "MAX_HEADER_LIST_SIZE" => {
                b.max_header_list_size(v);
            }
            other => {
                // ENABLE_CONNECT_PROTOCOL and NO_RFC7540_PRIORITIES have no
                // client setter in h2 yet.
                tracing::warn!("h2 setting {other} not supported, not sent");
                continue;
            }
        }
        if let Some(code) = s.code() {
            order.push(code);
        }
    }
    b.settings_order(order);

    if profile.http2_window_update > 0 {
        let target = DEFAULT_CONN_WINDOW
            .checked_add(profile.http2_window_update)
            .filter(|t| *t <= (1 << 31) - 1)
            .ok_or_else(|| {
                NetError::Profile(format!(
                    "http2_window_update {} too large",
                    profile.http2_window_update
                ))
            })?;
        b.initial_connection_window_size(target);
    }

    b.pseudo_header_order(pseudo_order(&profile.http2_pseudo_header_order)?);

    if !profile.http2_priority.frames.is_empty() {
        tracing::warn!("h2 PRIORITY frames in profile are not sent yet");
    }
    Ok(b)
}

/// Parse `[":method", ":authority", ":scheme", ":path"]` style lists.
pub fn pseudo_order(names: &[String]) -> Result<[PseudoHeader; 4], NetError> {
    let parse = |n: &str| match n {
        ":method" | "m" => Ok(PseudoHeader::Method),
        ":scheme" | "s" => Ok(PseudoHeader::Scheme),
        ":authority" | "a" => Ok(PseudoHeader::Authority),
        ":path" | "p" => Ok(PseudoHeader::Path),
        other => Err(NetError::Profile(format!("unknown pseudo-header {other}"))),
    };
    let [a, b, c, d] = names else {
        return Err(NetError::Profile(
            "http2_pseudo_header_order must have 4 entries".into(),
        ));
    };
    let order = [parse(a)?, parse(b)?, parse(c)?, parse(d)?];
    for (i, p) in order.iter().enumerate() {
        if order[..i].contains(p) {
            return Err(NetError::Profile(format!("duplicate pseudo-header {p:?}")));
        }
    }
    Ok(order)
}

/// Absolute-form request for h2 with headers in the given order.
fn build_request(req: &Request) -> Result<http::Request<()>, NetError> {
    let url = &req.url;
    let host = url
        .host_str()
        .ok_or_else(|| NetError::Http(format!("no host in {url}")))?;
    let authority = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let path = if path.is_empty() { "/" } else { path };
    let uri: Uri = Uri::builder()
        .scheme(url.scheme())
        .authority(authority)
        .path_and_query(path)
        .build()?;

    let mut out = http::Request::builder()
        .method(req.method.clone())
        .uri(uri)
        .version(Version::HTTP_2)
        .body(())?;
    *out.headers_mut() = header_map(&req.headers)?;
    Ok(out)
}

fn header_map(headers: &OrderedHeaders) -> Result<HeaderMap, NetError> {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| NetError::Http(format!("header name {name:?}: {e}")))?;
        if CONNECTION_SPECIFIC.contains(&name.as_str())
            || (name == http::header::TE && !value.eq_ignore_ascii_case("trailers"))
        {
            tracing::debug!("dropping {name} for h2");
            continue;
        }
        let value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|e| NetError::Http(format!("header {name}: {e}")))?;
        map.append(name, value);
    }
    Ok(map)
}

/// `HeaderMap` → ordered pairs. Non-UTF-8 values are decoded lossily.
pub(crate) fn ordered(map: &HeaderMap) -> OrderedHeaders {
    map.iter()
        .map(|(n, v)| {
            (
                n.as_str().to_owned(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;
    use tokio::net::{TcpListener, TcpStream};

    const FIREFOX_WIN10: &str = include_str!("../../../profiles/tls/firefox_128_win10.json");

    fn firefox() -> ClientHelloProfile {
        ClientHelloProfile::from_json(FIREFOX_WIN10).unwrap()
    }

    /// Chrome-like H2 values on top of the Firefox capture (no Chrome
    /// capture in the repo yet).
    fn chrome_like() -> ClientHelloProfile {
        let mut p = firefox();
        p.http2_settings.ordered_params = [
            ("HEADER_TABLE_SIZE", 65_536),
            ("ENABLE_PUSH", 0),
            ("INITIAL_WINDOW_SIZE", 6_291_456),
            ("MAX_HEADER_LIST_SIZE", 262_144),
        ]
        .into_iter()
        .map(|(id, value)| crate::profile::H2Setting {
            id: id.into(),
            value,
        })
        .collect();
        p.http2_window_update = 15_663_105;
        p.http2_pseudo_header_order = [":method", ":authority", ":scheme", ":path"]
            .map(String::from)
            .to_vec();
        p
    }

    fn get(port: u16, path: &str) -> Request {
        let mut r = Request::get(format!("http://127.0.0.1:{port}{path}").parse().unwrap());
        r.headers = vec![
            ("user-agent".into(), "lb-test".into()),
            ("accept".into(), "*/*".into()),
            ("x-order-b".into(), "1".into()),
            ("x-order-a".into(), "2".into()),
            ("connection".into(), "keep-alive".into()),
        ];
        r
    }

    #[test]
    fn header_map_keeps_insertion_order() {
        let h: OrderedHeaders = ["zeta", "alpha", "mid", "beta"]
            .iter()
            .map(|n| (n.to_string(), "v".to_string()))
            .collect();
        let map = header_map(&h).unwrap();
        let names: Vec<_> = map.keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["zeta", "alpha", "mid", "beta"]);
    }

    #[test]
    fn parses_pseudo_order() {
        assert_eq!(
            pseudo_order(&firefox().http2_pseudo_header_order).unwrap(),
            [
                PseudoHeader::Method,
                PseudoHeader::Path,
                PseudoHeader::Authority,
                PseudoHeader::Scheme
            ]
        );
        let dup = [":method", ":method", ":scheme", ":path"].map(String::from);
        assert!(pseudo_order(&dup).is_err());
    }

    /// End to end against upstream-behaving `h2::server`.
    #[tokio::test]
    async fn round_trip_against_h2_server() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();

        let server = tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut conn = h2::server::handshake(tcp).await.unwrap();
            let (req, mut respond) = conn.accept().await.unwrap().unwrap();
            let (parts, mut body) = req.into_parts();
            let mut got = Vec::new();
            while let Some(chunk) = body.data().await {
                got.extend_from_slice(&chunk.unwrap());
            }
            let resp = http::Response::builder()
                .status(201)
                .header("x-b", "1")
                .header("x-a", "2")
                .body(())
                .unwrap();
            let mut send = respond.send_response(resp, false).unwrap();
            send.send_data(Bytes::from_static(b"hello h2"), true)
                .unwrap();
            // Drive the connection until the client goes away.
            while let Some(r) = conn.accept().await {
                if r.is_err() {
                    break;
                }
            }
            (parts, got)
        });

        let mut req = get(port, "/echo?q=1");
        req.method = http::Method::POST;
        req.body = Bytes::from_static(b"payload");
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let resp = send(tcp, &req, &firefox()).await.unwrap();

        assert_eq!(resp.status, 201);
        assert_eq!(resp.version, HttpVersion::Http2);
        assert_eq!(&resp.body[..], b"hello h2");
        let names: Vec<_> = resp.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["x-b", "x-a"]);

        let (parts, got) = server.await.unwrap();
        assert_eq!(parts.method, http::Method::POST);
        assert_eq!(parts.uri.path_and_query().unwrap().as_str(), "/echo?q=1");
        assert_eq!(
            parts.uri.authority().unwrap().as_str(),
            format!("127.0.0.1:{port}")
        );
        assert_eq!(parts.headers["user-agent"], "lb-test");
        assert!(parts.headers.get("connection").is_none());
        let names: Vec<_> = parts.headers.keys().map(|k| k.as_str()).collect();
        assert_eq!(names, ["user-agent", "accept", "x-order-b", "x-order-a"]);
        assert_eq!(got, b"payload");
    }

    #[tokio::test]
    async fn body_cap_is_enforced() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            let (tcp, _) = listener.accept().await.unwrap();
            let mut conn = h2::server::handshake(tcp).await.unwrap();
            let (_req, mut respond) = conn.accept().await.unwrap().unwrap();
            let resp = http::Response::builder().status(200).body(()).unwrap();
            let mut send = respond.send_response(resp, false).unwrap();
            send.send_data(Bytes::from(vec![b'x'; 1000]), true).unwrap();
            while let Some(r) = conn.accept().await {
                if r.is_err() {
                    break;
                }
            }
        });
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let err = send_with_limit(tcp, &get(port, "/"), &firefox(), 100)
            .await
            .unwrap_err();
        assert!(matches!(err, NetError::BodyTooLarge { limit: 100 }));
    }

    /// What a raw server sees before it answers: SETTINGS, WINDOW_UPDATE and
    /// the first bytes of the HEADERS block.
    struct Preface {
        settings: Vec<(u16, u32)>,
        window_update: u32,
        hpack: Vec<u8>,
    }

    async fn capture_preface(profile: ClientHelloProfile) -> (Preface, u16) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let client = tokio::spawn(async move {
            let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
            // The raw server never answers; the error is expected.
            let _ = send(tcp, &get(port, "/"), &profile).await;
        });

        let (mut tcp, _) = listener.accept().await.unwrap();
        let mut magic = [0u8; 24];
        tcp.read_exact(&mut magic).await.unwrap();
        assert_eq!(&magic, b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n");

        let mut frames = Vec::new();
        for _ in 0..3 {
            let mut head = [0u8; 9];
            tcp.read_exact(&mut head).await.unwrap();
            let len = u32::from_be_bytes([0, head[0], head[1], head[2]]) as usize;
            let mut payload = vec![0u8; len];
            tcp.read_exact(&mut payload).await.unwrap();
            frames.push((head[3], payload));
        }
        drop(tcp);
        let _ = client.await;

        assert_eq!(frames[0].0, 0x4, "first frame is SETTINGS");
        let settings = frames[0]
            .1
            .chunks(6)
            .map(|c| {
                (
                    u16::from_be_bytes([c[0], c[1]]),
                    u32::from_be_bytes([c[2], c[3], c[4], c[5]]),
                )
            })
            .collect();
        assert_eq!(frames[1].0, 0x8, "second frame is WINDOW_UPDATE");
        let w = &frames[1].1;
        let window_update = u32::from_be_bytes([w[0], w[1], w[2], w[3]]) & 0x7fff_ffff;
        assert_eq!(frames[2].0, 0x1, "third frame is HEADERS");
        (
            Preface {
                settings,
                window_update,
                hpack: frames[2].1.clone(),
            },
            port,
        )
    }

    /// Pseudo-header sequence in an HPACK block written by h2 for a plain
    /// `GET http://127.0.0.1:port/`: `:method GET` = 0x82, `:path /` = 0x84,
    /// `:scheme http` = 0x86, `:authority` = literal with name index 1 (0x41).
    fn pseudo_letters(block: &[u8]) -> String {
        let mut out = String::new();
        let mut i = 0;
        while out.len() < 4 {
            match block[i] {
                0x82 => out.push('m'),
                0x84 => out.push('p'),
                0x86 => out.push('s'),
                0x41 => {
                    out.push('a');
                    let len = (block[i + 1] & 0x7f) as usize;
                    i += 1 + len;
                }
                b => panic!("unexpected hpack byte {b:#x} at {i}"),
            }
            i += 1;
        }
        out
    }

    #[tokio::test]
    async fn firefox_preface_on_the_wire() {
        let p = firefox();
        let (pre, _) = capture_preface(p.clone()).await;
        let want: Vec<_> = p
            .http2_settings
            .ordered_params
            .iter()
            .map(|s| (s.code().unwrap(), s.value))
            .collect();
        assert_eq!(pre.settings, want);
        assert_eq!(pre.window_update, 12_517_377);
        assert_eq!(pseudo_letters(&pre.hpack), "mpas");
    }

    #[tokio::test]
    async fn chrome_preface_on_the_wire() {
        let (pre, _) = capture_preface(chrome_like()).await;
        assert_eq!(
            pre.settings,
            [(1, 65_536), (2, 0), (4, 6_291_456), (6, 262_144)]
        );
        assert_eq!(pre.window_update, 15_663_105);
        assert_eq!(pseudo_letters(&pre.hpack), "masp");
    }
}
