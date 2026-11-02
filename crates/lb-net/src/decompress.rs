//! Response body decoding: gzip, deflate, brotli, zstd.
//!
//! Owner: M1 task 4.

use std::io::{self, Read};

use bytes::Bytes;
use flate2::read::{DeflateDecoder, GzDecoder, ZlibDecoder};

use crate::error::NetError;

/// Decode `body` according to a `Content-Encoding` value. `identity` and an
/// empty value return the input unchanged.
///
/// The value may be a comma-separated list (`gzip, br`): encodings were
/// applied left to right by the server, so they are undone right to left.
/// Unknown encodings are an error (`NetError::Decode`).
pub fn decode(encoding: &str, body: Bytes) -> Result<Bytes, NetError> {
    let mut body = body;
    for enc in encoding.rsplit(',') {
        let enc = enc.trim().to_ascii_lowercase();
        body = decode_one(&enc, body)?;
    }
    Ok(body)
}

fn decode_one(enc: &str, body: Bytes) -> Result<Bytes, NetError> {
    let fail = |source: io::Error| NetError::Decode {
        encoding: enc.to_owned(),
        source,
    };
    match enc {
        "" | "identity" => Ok(body),
        "gzip" | "x-gzip" => read_all(GzDecoder::new(&body[..])).map_err(fail),
        "deflate" => {
            // HTTP says zlib-wrapped, but some servers send raw deflate.
            match read_all(ZlibDecoder::new(&body[..])) {
                Ok(out) => Ok(out),
                Err(_) => read_all(DeflateDecoder::new(&body[..])).map_err(fail),
            }
        }
        "br" => read_all(brotli::Decompressor::new(&body[..], 4096)).map_err(fail),
        "zstd" => {
            let dec = zstd::stream::read::Decoder::new(&body[..]).map_err(fail)?;
            read_all(dec).map_err(fail)
        }
        other => Err(NetError::Decode {
            encoding: other.to_owned(),
            source: io::Error::new(io::ErrorKind::InvalidInput, "unknown content-encoding"),
        }),
    }
}

fn read_all(mut r: impl Read) -> io::Result<Bytes> {
    let mut out = Vec::new();
    r.read_to_end(&mut out)?;
    Ok(Bytes::from(out))
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
