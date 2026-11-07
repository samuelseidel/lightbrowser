//! Image decoding for `<img>` / `Image` and `drawImage`: PNG, JPEG, GIF
//! (first frame), WebP, ICO/BMP through the `image` crate. SVG is only
//! sized (root `width` / `height` / `viewBox`) and draws as nothing.
//!
//! Loading only reads the dimensions ([`probe`]); pixels are decoded on
//! first draw ([`decode`]) so pages that never paint images pay nothing.

use std::io::Cursor;

use tiny_skia::{IntSize, Pixmap};

/// Images larger than this (either side) are treated as broken.
pub const MAX_SIDE: u32 = 16384;

/// What a probe found.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageInfo {
    pub width: u32,
    pub height: u32,
    /// SVG: sized, but no pixels.
    pub vector: bool,
}

/// Why an image cannot be used.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError(pub String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DecodeError {}

fn err(e: impl std::fmt::Display) -> DecodeError {
    DecodeError(e.to_string())
}

/// True if the bytes (or the `Content-Type`) say SVG.
pub fn is_svg(bytes: &[u8], content_type: Option<&str>) -> bool {
    if content_type.is_some_and(|c| {
        c.split(';')
            .next()
            .is_some_and(|m| m.trim().eq_ignore_ascii_case("image/svg+xml"))
    }) {
        return true;
    }
    let head = String::from_utf8_lossy(&bytes[..bytes.len().min(1024)]).to_ascii_lowercase();
    head.trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with('<')
        && head.contains("<svg")
}

/// Dimensions of an encoded image.
pub fn probe(bytes: &[u8], content_type: Option<&str>) -> Result<ImageInfo, DecodeError> {
    if is_svg(bytes, content_type) {
        let (width, height) = svg_size(&String::from_utf8_lossy(bytes))?;
        return Ok(ImageInfo {
            width,
            height,
            vector: true,
        });
    }
    let reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(err)?;
    if reader.format().is_none() {
        return Err(DecodeError("unknown image format".into()));
    }
    let (width, height) = reader.into_dimensions().map_err(err)?;
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(DecodeError(format!("bad image size {width}x{height}")));
    }
    Ok(ImageInfo {
        width,
        height,
        vector: false,
    })
}

/// Decodes to a premultiplied pixmap. SVG gives a transparent pixmap of
/// its size.
pub fn decode(bytes: &[u8], content_type: Option<&str>) -> Result<Pixmap, DecodeError> {
    let info = probe(bytes, content_type)?;
    if info.vector {
        return Pixmap::new(info.width.max(1), info.height.max(1))
            .ok_or_else(|| DecodeError("empty".into()));
    }
    let img = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(err)?
        .decode()
        .map_err(err)?
        .to_rgba8();
    let (w, h) = img.dimensions();
    let mut data = img.into_raw();
    premultiply(&mut data);
    Pixmap::from_vec(
        data,
        IntSize::from_wh(w, h).ok_or_else(|| err("empty image"))?,
    )
    .ok_or_else(|| err("bad pixmap"))
}

/// Unpremultiplied RGBA8 → premultiplied, in place.
pub fn premultiply(data: &mut [u8]) {
    for p in data.chunks_exact_mut(4) {
        let a = u16::from(p[3]);
        if a < 255 {
            for c in &mut p[..3] {
                *c = ((u16::from(*c) * a + 127) / 255) as u8;
            }
        }
    }
}

/// Natural size of an SVG document: root `width`/`height` in px (or
/// unitless), else 300 wide with the `viewBox` ratio, else 300x150.
fn svg_size(text: &str) -> Result<(u32, u32), DecodeError> {
    let start = text
        .find("<svg")
        .ok_or_else(|| DecodeError("no svg root".into()))?;
    let end = text[start..]
        .find('>')
        .map(|e| start + e)
        .ok_or_else(|| DecodeError("bad svg root".into()))?;
    let tag = &text[start..end];
    let attr = |name: &str| -> Option<String> {
        let mut rest = tag;
        while let Some(p) = rest.find(name) {
            let before = rest[..p].chars().last();
            let after = rest[p + name.len()..].trim_start();
            if before.is_some_and(char::is_whitespace)
                && let Some(v) = after.strip_prefix('=')
            {
                let v = v.trim_start();
                let q = v.chars().next()?;
                if q == '"' || q == '\'' {
                    let inner = &v[1..];
                    return inner.find(q).map(|e| inner[..e].to_owned());
                }
            }
            rest = &rest[p + name.len()..];
        }
        None
    };
    let len = |v: Option<String>| -> Option<f32> {
        let v = v?;
        let v = v.trim();
        let num = v.strip_suffix("px").unwrap_or(v).trim();
        num.parse::<f32>()
            .ok()
            .filter(|n| n.is_finite() && *n > 0.0)
    };
    let (w, h) = (len(attr("width")), len(attr("height")));
    let ratio = attr("viewBox").and_then(|vb| {
        let n: Vec<f32> = vb
            .split(|c: char| c == ',' || c.is_whitespace())
            .filter(|s| !s.is_empty())
            .filter_map(|s| s.parse().ok())
            .collect();
        (n.len() == 4 && n[2] > 0.0 && n[3] > 0.0).then(|| n[2] / n[3])
    });
    let (w, h) = match (w, h, ratio) {
        (Some(w), Some(h), _) => (w, h),
        (Some(w), None, Some(r)) => (w, w / r),
        (None, Some(h), Some(r)) => (h * r, h),
        (Some(w), None, None) => (w, 150.0),
        (None, Some(h), None) => (300.0, h),
        (None, None, Some(r)) => (300.0, 300.0 / r),
        (None, None, None) => (300.0, 150.0),
    };
    let (w, h) = (w.round() as u32, h.round() as u32);
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(DecodeError(format!("bad svg size {w}x{h}")));
    }
    Ok((w, h))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn png(w: u32, h: u32) -> Vec<u8> {
        let mut b = crate::Bitmap::new(w, h);
        for p in b.data.chunks_exact_mut(4) {
            p.copy_from_slice(&[200, 100, 50, 128]);
        }
        crate::encode::encode(&b, "image/png", None)
    }

    #[test]
    fn probe_and_decode_png() {
        let bytes = png(7, 3);
        assert_eq!(
            probe(&bytes, Some("image/png")).unwrap(),
            ImageInfo {
                width: 7,
                height: 3,
                vector: false
            }
        );
        let pm = decode(&bytes, None).unwrap();
        assert_eq!((pm.width(), pm.height()), (7, 3));
        let p = pm.pixel(0, 0).unwrap();
        assert_eq!((p.red(), p.alpha()), (100, 128));
    }

    #[test]
    fn other_formats() {
        let mut b = crate::Bitmap::new(4, 5);
        b.data.fill(255);
        for ty in ["image/jpeg", "image/webp"] {
            let bytes = crate::encode::encode(&b, ty, None);
            let i = probe(&bytes, None).unwrap();
            assert_eq!((i.width, i.height), (4, 5), "{ty}");
            assert!(decode(&bytes, None).is_ok());
        }
        let gif = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\xff\xff\xff\x00\x00\x00!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";
        assert!(probe(gif, None).is_ok_and(|i| i.width == 1));
        assert!(decode(gif, None).is_ok());
        assert!(probe(b"not an image", None).is_err());
        assert!(probe(b"", Some("image/png")).is_err());
    }

    #[test]
    fn svg_sizes() {
        let s = |t: &str| probe(t.as_bytes(), None).map(|i| (i.width, i.height, i.vector));
        assert_eq!(
            s("<svg xmlns='http://www.w3.org/2000/svg' width=\"320\" height=\"220\"></svg>"),
            Ok((320, 220, true))
        );
        assert_eq!(
            s("<?xml version='1.0'?>\n<svg viewBox='0 0 40 20'/>"),
            Ok((300, 150, true))
        );
        assert_eq!(
            s("<svg width='100px' viewBox='0 0 40 20'/>"),
            Ok((100, 50, true))
        );
        assert_eq!(s("<svg/>"), Ok((300, 150, true)));
        assert_eq!(
            probe(b"<x/>", Some("image/svg+xml")),
            Err(DecodeError("no svg root".into()))
        );
    }
}
