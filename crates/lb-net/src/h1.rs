//! HTTP/1.1 over an established TLS stream (hyper 1.x).
//!
//! Sends `req` with its headers in the given order and returns the raw
//! (still compressed) response.
//!
//! Header names arrive lowercase (they are shared with HTTP/2). hyper keeps
//! the order of a `HeaderMap` but cannot write arbitrary case: its per-name
//! case map is crate-private. We use `title_case_headers`, which writes
//! `User-Agent`, `Accept-Language` and so on like both browsers do, but also
//! `Sec-Ch-Ua` where Chrome writes `sec-ch-ua`, `Te` where Firefox writes
//! `TE` and `Dnt` for `DNT`. Exact per-name case needs our own h1 head
//! writer or a hyper patch; tracked as an M1 open issue.

use bytes::{Bytes, BytesMut};
use http::{HeaderMap, HeaderName, HeaderValue, Uri, Version};
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1;
use hyper_util::rt::TokioIo;

use crate::HttpVersion;
use crate::error::NetError;
use crate::fetch::{RawResponse, Request};
use crate::h2::DEFAULT_MAX_BODY;
use crate::headers::OrderedHeaders;

/// Send `req` with a body cap of [`DEFAULT_MAX_BODY`].
pub async fn send<S>(io: S, req: &Request) -> Result<RawResponse, NetError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    send_with_limit(io, req, DEFAULT_MAX_BODY).await
}

/// Send `req` over `io` as HTTP/1.1 and read the response. Adds `Host` first
/// if `req` has none, and `Content-Length` after `Host`/`Connection` for a
/// non-empty body without one. Fails with [`NetError::BodyTooLarge`] once the
/// body exceeds `max_body` bytes.
pub async fn send_with_limit<S>(
    io: S,
    req: &Request,
    max_body: usize,
) -> Result<RawResponse, NetError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    let request = build_request(req)?;

    let (mut sender, conn) = http1::Builder::new()
        .title_case_headers(true)
        .handshake::<_, Full<Bytes>>(TokioIo::new(io))
        .await?;
    // Ends once `sender` is dropped and the exchange is done, or on error.
    tokio::spawn(async move {
        if let Err(e) = conn.await {
            tracing::debug!("h1 connection ended: {e}");
        }
    });

    let response = sender.send_request(request).await?;
    let (parts, mut body) = response.into_parts();

    let mut buf = BytesMut::new();
    while let Some(frame) = body.frame().await {
        if let Ok(data) = frame?.into_data() {
            if buf.len() + data.len() > max_body {
                return Err(NetError::BodyTooLarge { limit: max_body });
            }
            buf.extend_from_slice(&data);
        }
    }

    Ok(RawResponse {
        status: parts.status,
        headers: crate::h2::ordered(&parts.headers),
        body: buf.freeze(),
        version: HttpVersion::Http1,
    })
}

/// Origin-form request with headers in wire order.
fn build_request(req: &Request) -> Result<http::Request<Full<Bytes>>, NetError> {
    let url = &req.url;
    let host = url
        .host_str()
        .ok_or_else(|| NetError::Http(format!("no host in {url}")))?;
    let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let path = if path.is_empty() { "/" } else { path };
    let uri: Uri = path
        .parse()
        .map_err(|e| NetError::Http(format!("request target {path:?}: {e}")))?;

    let mut headers = req.headers.clone();
    if !has(&headers, "host") {
        let value = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        headers.insert(0, ("host".into(), value));
    }
    if !req.body.is_empty()
        && !has(&headers, "content-length")
        && !has(&headers, "transfer-encoding")
    {
        // Browsers put it right after Host / Connection.
        let at = headers
            .iter()
            .take_while(|(n, _)| {
                n.eq_ignore_ascii_case("host") || n.eq_ignore_ascii_case("connection")
            })
            .count();
        headers.insert(at, ("content-length".into(), req.body.len().to_string()));
    }

    let mut out = http::Request::builder()
        .method(req.method.clone())
        .uri(uri)
        .version(Version::HTTP_11)
        .body(Full::new(req.body.clone()))?;
    *out.headers_mut() = header_map(&headers)?;
    Ok(out)
}

fn has(headers: &OrderedHeaders, name: &str) -> bool {
    headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

fn header_map(headers: &OrderedHeaders) -> Result<HeaderMap, NetError> {
    let mut map = HeaderMap::with_capacity(headers.len());
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes())
            .map_err(|e| NetError::Http(format!("header name {name:?}: {e}")))?;
        let value = HeaderValue::from_bytes(value.as_bytes())
            .map_err(|e| NetError::Http(format!("header {name}: {e}")))?;
        map.append(name, value);
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpListener, TcpStream};

    /// A plain-TCP server that records the raw request head and replies with
    /// `reply`. Returns the port and a handle yielding the request bytes.
    async fn raw_server(reply: &'static [u8]) -> (u16, tokio::task::JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let handle = tokio::spawn(async move {
            let (mut tcp, _) = listener.accept().await.unwrap();
            let mut got = Vec::new();
            let mut buf = [0u8; 4096];
            loop {
                let n = tcp.read(&mut buf).await.unwrap();
                got.extend_from_slice(&buf[..n]);
                if n == 0 || got.windows(4).any(|w| w == b"\r\n\r\n") {
                    break;
                }
            }
            tcp.write_all(reply).await.unwrap();
            tcp.shutdown().await.unwrap();
            got
        });
        (port, handle)
    }

    fn get(port: u16) -> Request {
        let mut r = Request::get(
            format!("http://127.0.0.1:{port}/a/b?c=d#frag")
                .parse()
                .unwrap(),
        );
        r.headers = vec![
            ("connection".into(), "keep-alive".into()),
            ("user-agent".into(), "lb-test".into()),
            ("accept".into(), "*/*".into()),
            ("accept-language".into(), "en-US".into()),
        ];
        r
    }

    #[tokio::test]
    async fn sends_ordered_title_cased_head() {
        let (port, server) = raw_server(
            b"HTTP/1.1 200 OK\r\nX-B: 1\r\nContent-Type: text/plain\r\nX-A: 2\r\nContent-Length: 5\r\n\r\nhello",
        )
        .await;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let resp = send(tcp, &get(port)).await.unwrap();

        let head = String::from_utf8(server.await.unwrap()).unwrap();
        assert_eq!(
            head,
            format!(
                "GET /a/b?c=d HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: keep-alive\r\n\
                 User-Agent: lb-test\r\nAccept: */*\r\nAccept-Language: en-US\r\n\r\n"
            )
        );

        assert_eq!(resp.status, 200);
        assert_eq!(resp.version, HttpVersion::Http1);
        assert_eq!(&resp.body[..], b"hello");
        let names: Vec<_> = resp.headers.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["x-b", "content-type", "x-a", "content-length"]);
    }

    #[tokio::test]
    async fn post_adds_content_length_after_connection() {
        let (port, server) =
            raw_server(b"HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n").await;
        let mut req = get(port);
        req.method = http::Method::POST;
        req.body = Bytes::from_static(b"k=v");
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let resp = send(tcp, &req).await.unwrap();
        assert_eq!(resp.status, 204);

        let raw = server.await.unwrap();
        let head = String::from_utf8_lossy(&raw);
        assert!(
            head.starts_with(&format!(
                "POST /a/b?c=d HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: keep-alive\r\n\
                 Content-Length: 3\r\nUser-Agent: lb-test\r\n"
            )),
            "{head}"
        );
    }

    #[tokio::test]
    async fn reads_chunked_body_and_enforces_cap() {
        const CHUNKED: &[u8] =
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n";
        let (port, server) = raw_server(CHUNKED).await;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let resp = send(tcp, &get(port)).await.unwrap();
        assert_eq!(&resp.body[..], b"hello world");
        server.await.unwrap();

        let (port, server) = raw_server(CHUNKED).await;
        let tcp = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
        let err = send_with_limit(tcp, &get(port), 8).await.unwrap_err();
        assert!(matches!(err, NetError::BodyTooLarge { limit: 8 }));
        server.await.unwrap();
    }
}
