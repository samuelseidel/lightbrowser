//! Per-persona rasterization variation (ADR 0038, replacing ADR 0014's
//! readback noise).
//!
//! Real machines differ in how they rasterize, not in what readback
//! returns: every readback path of one canvas (`getImageData`,
//! `toDataURL`, `toBlob`, `convertToBlob`, `drawImage` of the canvas,
//! `putImageData` round trips, workers) gives the bitmap's exact values.
//! So the persona's variation goes into rasterization, where it only
//! moves edge coverage:
//!
//! - Canvas text ([`text_bias`], from `canvas_seed`): after glyph origins
//!   are snapped to the device grid as Chrome's glyph cache does (x to a
//!   quarter pixel, y to a whole pixel; see `font::GlyphGrid`), every
//!   glyph and emoji is moved by the same sub-pixel offset of at most
//!   1/8 px, standing for the font rasterizer's hinting and outline
//!   placement. Translations by whole pixels or within one snapping step
//!   still give identical pixels, flat fills and opaque glyph interiors
//!   keep the exact fill colour, and `measureText` is unchanged.
//! - WebGL ([`webgl_bias`], from `webgl_noise_seed`): lb-gl decides which
//!   samples a triangle, line or point covers as if it lay up to 1/16 px
//!   away (`lb_gl::raster::Fixed::offset`), standing for a GPU's sub-pixel
//!   precision; depth and varyings are interpolated at the unmoved
//!   positions, so shader output inside primitives is unchanged. Every
//!   multisample position lies at least 1/8 px from a pixel edge, so
//!   pixel-aligned geometry covers exactly the same samples; only slanted
//!   edges change.
//!
//! Offsets are multiples of 1/256 px (tiny-skia and lb-gl resolve no
//! finer): 64 x 64 text offsets and 32 x 32 WebGL offsets. The same seed
//! gives the same offsets on every run.

use lb_persona::sfc32::Sfc32;

/// First output of an SFC32 seeded with `seed`.
fn draw(seed: [u8; 16]) -> u32 {
    Sfc32::new(seed).next_u32()
}

/// Device-space offset (x, y) of canvas glyphs, in [-1/8, 1/8) px.
pub fn text_bias(seed: [u8; 16]) -> (f64, f64) {
    let r = draw(seed);
    let k = |v: u32| f64::from((v & 63) as i32 - 32) / 256.0;
    (k(r), k(r >> 6))
}

/// Window-space offset (x, y) of WebGL vertices, in [-1/16, 1/16) px.
pub fn webgl_bias(seed: [u8; 16]) -> (f64, f64) {
    let r = draw(seed);
    let k = |v: u32| f64::from((v & 31) as i32 - 16) / 256.0;
    (k(r), k(r >> 5))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn offsets_are_small_stable_and_on_the_grid() {
        for s in 0..=255u8 {
            let seed = [s, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, s];
            let (tx, ty) = text_bias(seed);
            let (wx, wy) = webgl_bias(seed);
            assert_eq!((tx, ty), text_bias(seed));
            assert_eq!((wx, wy), webgl_bias(seed));
            for v in [tx, ty] {
                assert!((-0.125..0.125).contains(&v), "{v}");
                assert_eq!((v * 256.0).fract(), 0.0);
            }
            for v in [wx, wy] {
                assert!((-0.0625..0.0625).contains(&v), "{v}");
                assert_eq!((v * 256.0).fract(), 0.0);
            }
        }
    }

    /// The reference personas each get their own offsets.
    #[test]
    fn reference_personas_differ() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/reference/chrome"
        );
        let (mut text, mut gl, mut n) = (HashSet::new(), HashSet::new(), 0);
        for entry in std::fs::read_dir(dir).unwrap() {
            let p = lb_persona::PersonaProfile::from_path(entry.unwrap().path()).unwrap();
            let t = text_bias(p.fingerprint_seeds.canvas_bytes().unwrap());
            let w = webgl_bias(p.fingerprint_seeds.webgl_bytes().unwrap());
            text.insert(((t.0 * 256.0) as i32, (t.1 * 256.0) as i32));
            gl.insert(((w.0 * 256.0) as i32, (w.1 * 256.0) as i32));
            n += 1;
        }
        assert_eq!(n, 8);
        assert_eq!(text.len(), n, "{text:?}");
        assert_eq!(gl.len(), n, "{gl:?}");
    }
}
