//! Response body decoding: gzip, deflate, brotli, zstd.
//!
//! Output is bounded while it is inflated: each `Content-Encoding` layer
//! stops once its output passes the limit (the context's `max_body`) and
//! fails with [`NetError::BodyTooLarge`], so a small compressed body (a
//! "bomb", or several stacked layers) never allocates more than the limit
//! plus one read buffer.
//!
//! Owner: M1 task 4.

use std::io::{self, Read};

use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};

use crate::error::NetError;
use crate::h2::DEFAULT_MAX_BODY;

/// Size of the buffer a decoder writes into before its output is copied
/// into the body.
const READ_STEP: usize = 64 * 1024;

/// [`decode_with_limit`] with the default body cap ([`DEFAULT_MAX_BODY`]).
pub fn decode(encoding: &str, body: Bytes) -> Result<Bytes, NetError> {
    decode_with_limit(encoding, body, DEFAULT_MAX_BODY)
}

/// Decode `body` according to a `Content-Encoding` value. `identity` and an
/// empty value return the input unchanged.
///
/// The value may be a comma-separated list (`gzip, br`): encodings were
/// applied left to right by the server, so they are undone right to left.
/// Unknown encodings are an error (`NetError::Decode`). Once any layer's
/// output exceeds `limit` bytes decoding stops with
/// [`NetError::BodyTooLarge`].
pub fn decode_with_limit(encoding: &str, body: Bytes, limit: usize) -> Result<Bytes, NetError> {
    let mut body = body;
    for enc in encoding.rsplit(',') {
        let enc = enc.trim().to_ascii_lowercase();
        body = decode_one(&enc, body, limit)?;
    }
    if body.len() > limit {
        return Err(NetError::BodyTooLarge { limit });
    }
    Ok(body)
}

/// Why a bounded read stopped.
enum ReadFail {
    Io(io::Error),
    TooLarge,
}

fn decode_one(enc: &str, body: Bytes, limit: usize) -> Result<Bytes, NetError> {
    let fail = |e: ReadFail| match e {
        ReadFail::Io(source) => NetError::Decode {
            encoding: enc.to_owned(),
            source,
        },
        ReadFail::TooLarge => NetError::BodyTooLarge { limit },
    };
    match enc {
        "" | "identity" => Ok(body),
        "gzip" | "x-gzip" => read_bounded(GzDecoder::new(&body[..]), limit).map_err(fail),
        "deflate" => {
            // HTTP says zlib-wrapped, but some servers send raw deflate.
            match read_bounded(ZlibDecoder::new(&body[..]), limit) {
                Ok(out) => Ok(out),
                Err(ReadFail::TooLarge) => Err(fail(ReadFail::TooLarge)),
                Err(ReadFail::Io(_)) => {
                    read_bounded(DeflateDecoder::new(&body[..]), limit).map_err(fail)
                }
            }
        }
        "br" => read_bounded(brotli::Decompressor::new(&body[..], 4096), limit).map_err(fail),
        "zstd" => {
            let dec =
                zstd::stream::read::Decoder::new(&body[..]).map_err(|e| fail(ReadFail::Io(e)))?;
            read_bounded(dec, limit).map_err(fail)
        }
        other => Err(NetError::Decode {
            encoding: other.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "unknown content-encoding"),
        }),
    }
}

/// Read `r` to the end, failing once more than `limit` bytes come out. The
/// output's capacity never exceeds `limit`, plus one [`READ_STEP`] buffer.
fn read_bounded(mut r: impl Read, limit: usize) -> Result<Bytes, ReadFail> {
    let mut out = Vec::new();
    let mut step = vec![0u8; READ_STEP];
    loop {
        let n = match r.read(&mut step) {
            Ok(0) => break,
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ReadFail::Io(e)),
        };
        if !push_bounded(&mut out, &step[..n], limit) {
            return Err(ReadFail::TooLarge);
        }
    }
    Ok(Bytes::from(out))
}

/// Append `data` to `out` unless that would make it longer than `limit`
/// (then `false`, `out` unchanged). Grows by doubling, but never past
/// `limit`, so a capped buffer is never allocated larger than its cap.
pub(crate) fn push_bounded(out: &mut Vec<u8>, data: &[u8], limit: usize) -> bool {
    let Some(need) = out.len().checked_add(data.len()).filter(|&n| n <= limit) else {
        return false;
    };
    if need > out.capacity() {
        let target = out
            .capacity()
            .saturating_mul(2)
            .max(READ_STEP)
            .max(need)
            .min(limit);
        out.reserve_exact(target - out.len());
    }
    out.extend_from_slice(data);
    true
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use flate2::Compression;
    use flate2::write::{DeflateEncoder, GzEncoder, ZlibEncoder};

    use super::*;

    const TEXT: &[u8] = b"<html><body>hello hello hello hello hello world</body></html>";

    fn gzip(d: &[u8]) -> Bytes {
        let mut e = GzEncoder::new(Vec::new(), Compression::default());
        e.write_all(d).unwrap();
        e.finish().unwrap().into()
    }
    fn zlib(d: &[u8]) -> Bytes {
        let mut e = ZlibEncoder::new(Vec::new(), Compression::default());
        e.write_all(d).unwrap();
        e.finish().unwrap().into()
    }
    fn raw_deflate(d: &[u8]) -> Bytes {
        let mut e = DeflateEncoder::new(Vec::new(), Compression::default());
        e.write_all(d).unwrap();
        e.finish().unwrap().into()
    }
    fn brotli_enc(d: &[u8]) -> Bytes {
        let mut out = Vec::new();
        {
            let mut w = brotli::CompressorWriter::new(&mut out, 4096, 5, 22);
            w.write_all(d).unwrap();
        }
        out.into()
    }
    fn zstd_enc(d: &[u8]) -> Bytes {
        zstd::encode_all(d, 3).unwrap().into()
    }

    #[test]
    fn passthrough() {
        for enc in ["", "identity", " Identity "] {
            assert_eq!(decode(enc, Bytes::from_static(TEXT)).unwrap(), TEXT);
        }
    }

    #[test]
    fn round_trips() {
        assert_eq!(decode("gzip", gzip(TEXT)).unwrap(), TEXT);
        assert_eq!(decode("x-gzip", gzip(TEXT)).unwrap(), TEXT);
        assert_eq!(decode("GZIP", gzip(TEXT)).unwrap(), TEXT);
        assert_eq!(decode("deflate", zlib(TEXT)).unwrap(), TEXT);
        assert_eq!(decode("deflate", raw_deflate(TEXT)).unwrap(), TEXT);
        assert_eq!(decode("br", brotli_enc(TEXT)).unwrap(), TEXT);
        assert_eq!(decode("zstd", zstd_enc(TEXT)).unwrap(), TEXT);
    }

    #[test]
    fn empty_bodies() {
        assert_eq!(decode("gzip", gzip(b"")).unwrap(), &b""[..]);
        assert_eq!(decode("br", brotli_enc(b"")).unwrap(), &b""[..]);
        assert_eq!(decode("zstd", zstd_enc(b"")).unwrap(), &b""[..]);
    }

    #[test]
    fn chain_applied_right_to_left() {
        // Server applied gzip first, then br: header is "gzip, br".
        let body = brotli_enc(&gzip(TEXT));
        assert_eq!(decode("gzip, br", body).unwrap(), TEXT);
        let body = zstd_enc(&zlib(TEXT));
        assert_eq!(decode("deflate,zstd", body).unwrap(), TEXT);
        // identity inside a chain is a no-op.
        assert_eq!(decode("identity, gzip", gzip(TEXT)).unwrap(), TEXT);
    }

    #[test]
    fn corrupt_input_reports_encoding() {
        for enc in ["gzip", "deflate", "br", "zstd"] {
            let err = decode(enc, Bytes::from_static(b"definitely not compressed")).unwrap_err();
            match err {
                NetError::Decode { encoding, .. } => assert_eq!(encoding, enc),
                other => panic!("unexpected error: {other:?}"),
            }
        }
    }

    #[test]
    fn truncated_gzip_fails() {
        let full = gzip(TEXT);
        let cut = full.slice(..full.len() - 6);
        assert!(matches!(decode("gzip", cut), Err(NetError::Decode { .. })));
    }

    /// A body of `n` zero bytes, compressed by `f`, written in steps so the
    /// input is never in memory at once.
    fn zeros_through<W: Write>(mut w: W, n: usize) -> W {
        let block = vec![0u8; 1 << 20];
        let mut left = n;
        while left > 0 {
            let k = left.min(block.len());
            w.write_all(&block[..k]).unwrap();
            left -= k;
        }
        w
    }

    #[test]
    fn every_codec_stops_at_the_limit() {
        // 16 MiB of zeros per codec, limit 1 MiB: each fails with the cap
        // error and never holds more than the limit.
        const N: usize = 16 << 20;
        const LIMIT: usize = 1 << 20;
        let gz = zeros_through(GzEncoder::new(Vec::new(), Compression::fast()), N)
            .finish()
            .unwrap();
        let zl = zeros_through(ZlibEncoder::new(Vec::new(), Compression::fast()), N)
            .finish()
            .unwrap();
        let raw = zeros_through(DeflateEncoder::new(Vec::new(), Compression::fast()), N)
            .finish()
            .unwrap();
        let mut br = Vec::new();
        drop(zeros_through(
            brotli::CompressorWriter::new(&mut br, 4096, 1, 22),
            N,
        ));
        let zs = zeros_through(zstd::Encoder::new(Vec::new(), 1).unwrap(), N)
            .finish()
            .unwrap();
        for (enc, body) in [
            ("gzip", gz),
            ("deflate", zl),
            ("deflate", raw),
            ("br", br),
            ("zstd", zs),
        ] {
            let small = body.len();
            match decode_with_limit(enc, body.into(), LIMIT) {
                Err(NetError::BodyTooLarge { limit }) => assert_eq!(limit, LIMIT, "{enc}"),
                other => panic!("{enc} ({small} bytes): {other:?}"),
            }
        }
        // Exactly at the limit is fine.
        let at = zeros_through(GzEncoder::new(Vec::new(), Compression::fast()), LIMIT)
            .finish()
            .unwrap();
        assert_eq!(
            decode_with_limit("gzip", at.into(), LIMIT).unwrap().len(),
            LIMIT
        );
    }

    #[test]
    fn push_bounded_never_grows_past_the_limit() {
        let mut v = Vec::new();
        let chunk = [7u8; 1000];
        while push_bounded(&mut v, &chunk, 100_500) {}
        assert_eq!(v.len(), 100_000);
        assert!(v.capacity() <= 100_500, "{}", v.capacity());
        assert!(!push_bounded(&mut v, &[0; 501], 100_500));
        assert!(push_bounded(&mut v, &[0; 500], 100_500));
        assert_eq!(v.len(), 100_500);
    }

    #[test]
    fn unknown_encoding_is_error() {
        match decode("compress", Bytes::from_static(b"x")).unwrap_err() {
            NetError::Decode { encoding, .. } => assert_eq!(encoding, "compress"),
            other => panic!("unexpected error: {other:?}"),
        }
        // Unknown member of a chain fails too.
        assert!(decode("gzip, snappy", gzip(TEXT)).is_err());
    }
}
