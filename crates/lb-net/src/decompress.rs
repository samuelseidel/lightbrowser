//! Response body decoding: gzip, deflate, brotli, zstd.
//!
//! Owner: M1 task 4.

use bytes::Bytes;

use crate::error::NetError;

/// Decode `body` according to a `Content-Encoding` value. `identity` and an
/// empty value return the input unchanged.
pub fn decode(_encoding: &str, body: Bytes) -> Result<Bytes, NetError> {
    Ok(body)
}
