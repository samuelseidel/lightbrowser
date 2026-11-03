//! HTTP/1.1 over an established stream: our own request writer and a small
//! response reader (httparse).
//!
//! Browsers write header names in a specific case (Chrome: `Accept-Language`
//! but `sec-ch-ua`) and hyper cannot be told to, so the request head is
//! written here byte for byte: request line, then the headers exactly as
//! given (name case and order included). The caller has already applied the
//! family's case (see [`crate::header_profile`]).
//!
//! One request per connection (`Connection: keep-alive` is sent because the
//! browser sends it, but the connection is dropped after the response).
//! Response headers come back lowercase, in arrival order. Bodies: 1xx
//! responses are skipped, then `Content-Length`, `Transfer-Encoding: chunked`
//! or read-to-close.

use bytes::{Bytes, BytesMut};
use http::{Method, StatusCode};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::HttpVersion;
use crate::error::NetError;
use crate::fetch::{RawResponse, Request};
use crate::h2::DEFAULT_MAX_BODY;
use crate::headers::OrderedHeaders;

/// Largest response head we accept.
const MAX_HEAD: usize = 64 * 1024;

/// Send `req` with a body cap of [`DEFAULT_MAX_BODY`].
pub async fn send<S>(io: S, req: &Request) -> Result<RawResponse, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    send_with_limit(io, req, DEFAULT_MAX_BODY).await
}

/// Send `req` over `io` as HTTP/1.1 and read the response. Adds `Host` first
/// if `req` has none, and `Content-Length` after `Host`/`Connection` for a
/// non-empty body without one. Fails with [`NetError::BodyTooLarge`] once the
/// body exceeds `max_body` bytes.
pub async fn send_with_limit<S>(
    mut io: S,
    req: &Request,
    max_body: usize,
) -> Result<RawResponse, NetError>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let mut out = request_head(req)?;
    out.extend_from_slice(&req.body);
    io.write_all(&out).await.map_err(NetError::Connect)?;
    io.flush().await.map_err(NetError::Connect)?;
    read_response(&mut io, &req.method, max_body).await
}

/// Request line and headers, ending with the blank line.
fn request_head(req: &Request) -> Result<Vec<u8>, NetError> {
    let url = &req.url;
    let host = url
        .host_str()
        .ok_or_else(|| NetError::Http(format!("no host in {url}")))?;
    let path = &url[url::Position::BeforePath..url::Position::AfterQuery];
    let path = if path.is_empty() { "/" } else { path };

    let mut headers = req.headers.clone();
    if !has(&headers, "host") {
        let value = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_owned(),
        };
        headers.insert(0, ("Host".into(), value));
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
        headers.insert(at, ("Content-Length".into(), req.body.len().to_string()));
    }

    let mut out = Vec::with_capacity(512);
    out.extend_from_slice(req.method.as_str().as_bytes());
    out.push(b' ');
    out.extend_from_slice(path.as_bytes());
    out.extend_from_slice(b" HTTP/1.1\r\n");
    for (name, value) in &headers {
        if name.is_empty()
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
        {
            return Err(NetError::Http(format!("header name {name:?}")));
        }
        if value.bytes().any(|b| b == b'\r' || b == b'\n' || b == 0) {
            return Err(NetError::Http(format!("header {name}: invalid value")));
        }
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(b": ");
        out.extend_from_slice(value.as_bytes());
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(b"\r\n");
    Ok(out)
}

fn has(headers: &OrderedHeaders, name: &str) -> bool {
    headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
}

/// Read one final response (skipping 1xx) and its body.
async fn read_response<S>(
    io: &mut S,
    method: &Method,
    max_body: usize,
) -> Result<RawResponse, NetError>
where
    S: AsyncRead + Unpin,
{
    let mut buf = BytesMut::with_capacity(8192);
    loop {
        // Head.
        let (status, headers, head_len) = loop {
            let mut raw = [httparse::EMPTY_HEADER; 128];
            let mut resp = httparse::Response::new(&mut raw);
            match resp.parse(&buf) {
                Ok(httparse::Status::Complete(n)) => {
                    let status = StatusCode::from_u16(resp.code.unwrap_or(0))
                        .map_err(|e| NetError::Http(format!("status line: {e}")))?;
                    let headers: OrderedHeaders = resp
                        .headers
                        .iter()
                        .map(|h| {
                            (
                                h.name.to_ascii_lowercase(),
                                String::from_utf8_lossy(h.value).into_owned(),
                            )
                        })
                        .collect();
                    break (status, headers, n);
                }
                Ok(httparse::Status::Partial) => {
                    if buf.len() > MAX_HEAD {
                        return Err(NetError::Http("response head too large".into()));
                    }
                    if io.read_buf(&mut buf).await.map_err(NetError::Connect)? == 0 {
                        return Err(NetError::Http(
                            "connection closed before the response head".into(),
                        ));
                    }
                }
                Err(e) => return Err(NetError::Http(format!("response head: {e}"))),
            }
        };
        let _ = buf.split_to(head_len);
        // 101 is a final answer for us; other 1xx are interim.
        if status.is_informational() && status != StatusCode::SWITCHING_PROTOCOLS {
            continue;
        }

        let no_body = *method == Method::HEAD
            || status.is_informational()
            || status == StatusCode::NO_CONTENT
            || status == StatusCode::NOT_MODIFIED;
        let body = if no_body {
            Bytes::new()
        } else if header(&headers, "transfer-encoding")
            .is_some_and(|v| v.to_ascii_lowercase().contains("chunked"))
        {
            read_chunked(io, buf, max_body).await?
        } else if let Some(len) = header(&headers, "content-length") {
            let len: usize = len
                .trim()
                .parse()
                .map_err(|_| NetError::Http(format!("bad content-length {len:?}")))?;
            if len > max_body {
                return Err(NetError::BodyTooLarge { limit: max_body });
            }
            while buf.len() < len {
                if io.read_buf(&mut buf).await.map_err(NetError::Connect)? == 0 {
                    return Err(NetError::Http("connection closed inside the body".into()));
                }
            }
            buf.truncate(len);
            buf.freeze()
        } else {
            // Until close.
            loop {
                if buf.len() > max_body {
                    return Err(NetError::BodyTooLarge { limit: max_body });
                }
                match io.read_buf(&mut buf).await {
                    Ok(0) => break,
                    Ok(_) => {}
                    // Servers that reset after the last byte are common.
                    Err(e) if e.kind() == std::io::ErrorKind::ConnectionReset => break,
                    Err(e) => return Err(NetError::Connect(e)),
                }
            }
            buf.freeze()
        };

        return Ok(RawResponse {
            status,
            headers,
            body,
            version: HttpVersion::Http1,
        });
    }
}

fn header<'a>(headers: &'a OrderedHeaders, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(n, _)| n == name)
        .map(|(_, v)| v.as_str())
}

/// Decode a chunked body. `buf` holds what was read past the head.
async fn read_chunked<S>(io: &mut S, mut buf: BytesMut, max_body: usize) -> Result<Bytes, NetError>
where
    S: AsyncRead + Unpin,
{
    let mut body = BytesMut::new();
    loop {
        // Chunk size line.
        let line_end = loop {
            if let Some(i) = buf.windows(2).position(|w| w == b"\r\n") {
                break i;
            }
            if buf.len() > MAX_HEAD {
                return Err(NetError::Http("chunk header too large".into()));
            }
            if io.read_buf(&mut buf).await.map_err(NetError::Connect)? == 0 {
                return Err(NetError::Http("connection closed inside a chunk".into()));
            }
        };
        let line = String::from_utf8_lossy(&buf[..line_end]).into_owned();
        let size_text = line.split(';').next().unwrap_or("").trim();
        let size = usize::from_str_radix(size_text, 16)
            .map_err(|_| NetError::Http(format!("bad chunk size {size_text:?}")))?;
        let _ = buf.split_to(line_end + 2);
        if size == 0 {
            // Trailers (ignored) end with an empty line; the connection is
            // dropped afterwards, so there is no need to read them.
            return Ok(body.freeze());
        }
        if body.len() + size > max_body {
            return Err(NetError::BodyTooLarge { limit: max_body });
        }
        while buf.len() < size + 2 {
            if io.read_buf(&mut buf).await.map_err(NetError::Connect)? == 0 {
                return Err(NetError::Http("connection closed inside a chunk".into()));
            }
        }
        body.extend_from_slice(&buf[..size]);
        let _ = buf.split_to(size + 2);
    }
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
            ("Connection".into(), "keep-alive".into()),
            ("User-Agent".into(), "lb-test".into()),
            ("sec-ch-ua".into(), "\"X\";v=\"1\"".into()),
            ("Accept".into(), "*/*".into()),
            ("Accept-Language".into(), "en-US".into()),
        ];
        r
    }

    #[tokio::test]
    async fn sends_head_with_exact_case_and_order() {
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
                 User-Agent: lb-test\r\nsec-ch-ua: \"X\";v=\"1\"\r\nAccept: */*\r\n\
                 Accept-Language: en-US\r\n\r\n"
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
