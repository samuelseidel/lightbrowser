//! HTTP/2 over an established TLS stream.
//!
//! Owner: M1 task 3. The connection preface (SETTINGS order and values, the
//! initial WINDOW_UPDATE) and the pseudo-header order come from the profile.
//! Upstream `h2` hard-codes both, so this module drives a vendored fork.

use crate::error::NetError;
use crate::fetch::{RawResponse, Request};
use crate::profile::ClientHelloProfile;

pub async fn send<S>(
    _io: S,
    _req: &Request,
    _profile: &ClientHelloProfile,
) -> Result<RawResponse, NetError>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin + Send + 'static,
{
    Err(NetError::Http(
        "h2::send not implemented (M1 task 3)".into(),
    ))
}
