//! HTTP/1.1 over an established TLS stream (hyper 1.x).
//!
//! Owner: M1 task 3. Sends `req` with its headers in the given order and
//! returns the raw (still compressed) response.

use crate::error::NetError;
use crate::fetch::{RawResponse, Request};

pub async fn send<S>(_io: S, _req: &Request) -> Result<RawResponse, NetError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    Err(NetError::Http(
        "h1::send not implemented (M1 task 3)".into(),
    ))
}
