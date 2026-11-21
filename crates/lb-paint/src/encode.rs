//! Image encoders for `toDataURL`, `toBlob` and `convertToBlob`.
//!
//! - `image/png` (default and fallback for unknown types).
//! - `image/jpeg`, quality 0..=1 (default 0.92, like Chrome); transparent
//!   pixels are composited onto black.
//! - `image/webp`: always lossless (VP8L); Chrome encodes lossy VP8 below
//!   quality 1.0. The output is a valid WebP, so support checks such as
//!   `toDataURL('image/webp').startsWith('data:image/webp')` pass.

use image::ImageEncoder;

/// An unpremultiplied RGBA8 image (what `getImageData` returns).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Bitmap {
    pub width: u32,
    pub height: u32,
    /// `width * height * 4` bytes, row-major RGBA.
    pub data: Vec<u8>,
}

impl Bitmap {
    /// A transparent bitmap.
    pub fn new(width: u32, height: u32) -> Bitmap {
        Bitmap {
            width,
            height,
            data: vec![0; width as usize * height as usize * 4],
        }
    }
}

/// The MIME type an encoder will actually produce for a requested type.
pub fn output_type(requested: &str) -> &'static str {
    match requested.trim().to_ascii_lowercase().as_str() {
        "image/jpeg" | "image/jpg" => "image/jpeg",
        "image/webp" => "image/webp",
        _ => "image/png",
    }
}

/// Encodes `bmp` as `mime` (see [`output_type`]). `quality` applies to
/// JPEG; values outside 0..=1 use the default.
pub fn encode(bmp: &Bitmap, mime: &str, quality: Option<f64>) -> Vec<u8> {
    let mut out = Vec::new();
    let (w, h) = (bmp.width, bmp.height);
    let res = match output_type(mime) {
        "image/jpeg" => {
            let q = quality.filter(|q| (0.0..=1.0).contains(q)).unwrap_or(0.92);
            let q = ((q * 100.0).round() as u8).clamp(1, 100);
            let rgb: Vec<u8> = bmp
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .flat_map(|p| {
                    let a = u16::from(p[3]);
                    [0, 1, 2].map(|c| ((u16::from(p[c]) * a + 127) / 255) as u8)
                })
                .collect();
            image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, q).write_image(
                &rgb,
                w,
                h,
                image::ExtendedColorType::Rgb8,
            )
        }
        "image/webp" => image::codecs::webp::WebPEncoder::new_lossless(&mut out).write_image(
            &bmp.data,
            w,
            h,
            image::ExtendedColorType::Rgba8,
        ),
        _ => image::codecs::png::PngEncoder::new_with_quality(
            &mut out,
            image::codecs::png::CompressionType::Default,
            image::codecs::png::FilterType::Adaptive,
        )
        .write_image(&bmp.data, w, h, image::ExtendedColorType::Rgba8),
    };
    if res.is_err() {
        out.clear();
    }
    out
}

/// `data:<mime>;base64,...`, or `data:,` for an empty bitmap (Chrome).
pub fn data_url(bmp: &Bitmap, mime: &str, quality: Option<f64>) -> String {
    if bmp.width == 0 || bmp.height == 0 {
        return "data:,".into();
    }
    let ty = output_type(mime);
    let bytes = encode(bmp, ty, quality);
    let mut s = String::with_capacity(bytes.len() * 4 / 3 + 32);
    s.push_str("data:");
    s.push_str(ty);
    s.push_str(";base64,");
    base64_into(&bytes, &mut s);
    s
}

/// Standard base64 with padding.
pub fn base64_into(bytes: &[u8], out: &mut String) {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for c in bytes.chunks(3) {
        let n = (u32::from(c[0]) << 16)
            | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
            | u32::from(*c.get(2).unwrap_or(&0));
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if c.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if c.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
}

/// Decodes standard or URL-safe base64, ignoring ASCII whitespace.
/// `None` on invalid input.
pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for b in s.bytes() {
        let v = match b {
            b'A'..=b'Z' => b - b'A',
            b'a'..=b'z' => b - b'a' + 26,
            b'0'..=b'9' => b - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => continue,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        let mut s = String::new();
        base64_into(b"any carnal pleas", &mut s);
        assert_eq!(s, "YW55IGNhcm5hbCBwbGVhcw==");
        assert_eq!(base64_decode(&s).unwrap(), b"any carnal pleas");
    }

    #[test]
    fn encoders_produce_their_formats() {
        let mut b = Bitmap::new(3, 2);
        b.data[3] = 255;
        assert!(data_url(&b, "", None).starts_with("data:image/png;base64,iVBORw0KGgo"));
        assert!(data_url(&b, "image/jpeg", Some(0.5)).starts_with("data:image/jpeg;base64,/9j/"));
        assert!(data_url(&b, "image/webp", None).starts_with("data:image/webp;base64,UklGR"));
        assert!(data_url(&b, "image/bogus", None).starts_with("data:image/png;"));
        assert_eq!(data_url(&Bitmap::new(0, 5), "", None), "data:,");
        // Deterministic.
        assert_eq!(encode(&b, "image/png", None), encode(&b, "image/png", None));
    }
}
