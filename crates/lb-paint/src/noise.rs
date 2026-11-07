//! Seeded readback noise (SFC32, `lb_persona::Sfc32`).
//!
//! Applied only to what leaves the canvas (`toDataURL`, `toBlob`,
//! `convertToBlob`, `getImageData`); the bitmap itself is never changed.
//!
//! Which pixels: a pixel is noised only when it is not fully transparent
//! and differs from at least one of its four neighbours inside the canvas
//! (an edge: anti-aliasing, text, shape borders), comparing every channel
//! without its lowest bit. Solid fills and blank canvases read back
//! exactly, as on a real GPU.
//!
//! How: for canvas row `y`, an `Sfc32` is seeded with the canvas seed whose
//! bytes 8..12 are XORed with the canvas width and bytes 12..16 with `y`
//! (big-endian). It yields one `u32` per pixel, `x = 0..`, whether or not
//! that pixel is noised, so a value depends only on (seed, width, x, y).
//! Bit `c` of it replaces the lowest bit of channel `c` (R, G, B; alpha is
//! never touched), so every change is ±1. Noise only rewrites low bits and
//! edges are found without them, so readback is idempotent: writing a
//! noised read back with `putImageData` and reading again gives the same
//! values (for opaque pixels; premultiplication rounds others, as in
//! Chrome).

use lb_persona::sfc32::Sfc32;

/// The noise of one persona (`fingerprint_seeds.canvas_seed`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CanvasNoise {
    seed: [u8; 16],
}

impl CanvasNoise {
    pub fn new(seed: [u8; 16]) -> Self {
        CanvasNoise { seed }
    }

    fn row_rng(&self, width: u32, y: u32) -> Sfc32 {
        let mut s = self.seed;
        for (i, b) in width.to_be_bytes().into_iter().enumerate() {
            s[8 + i] ^= b;
        }
        for (i, b) in y.to_be_bytes().into_iter().enumerate() {
            s[12 + i] ^= b;
        }
        Sfc32::new(s)
    }

    /// Noises `out`, an unpremultiplied RGBA copy of the canvas region at
    /// (`x0`, `y0`) with `out_w` columns. `canvas` is the premultiplied
    /// bitmap (`width` x `height`), used to find edges.
    #[allow(clippy::too_many_arguments)]
    pub fn apply(
        &self,
        canvas: &[u8],
        width: u32,
        height: u32,
        x0: i64,
        y0: i64,
        out_w: u32,
        out: &mut [u8],
    ) {
        if width == 0 || height == 0 || out_w == 0 {
            return;
        }
        let out_h = out.len() / (out_w as usize * 4);
        let px = |x: i64, y: i64| -> &[u8] {
            let i = (y as usize * width as usize + x as usize) * 4;
            &canvas[i..i + 4]
        };
        for oy in 0..out_h {
            let y = y0 + oy as i64;
            if y < 0 || y >= height as i64 {
                continue;
            }
            let cx0 = x0.max(0);
            let cx1 = (x0 + out_w as i64).min(width as i64);
            if cx0 >= cx1 {
                continue;
            }
            let mut rng = self.row_rng(width, y as u32);
            for _ in 0..cx0 {
                rng.next_u32();
            }
            for x in cx0..cx1 {
                let r = rng.next_u32();
                let p = px(x, y);
                if p[3] == 0 {
                    continue;
                }
                let differs = |q: &[u8]| q.iter().zip(p).any(|(a, b)| a >> 1 != b >> 1);
                let edge = (x > 0 && differs(px(x - 1, y)))
                    || (x + 1 < width as i64 && differs(px(x + 1, y)))
                    || (y > 0 && differs(px(x, y - 1)))
                    || (y + 1 < height as i64 && differs(px(x, y + 1)));
                if !edge {
                    continue;
                }
                let o = (oy * out_w as usize + (x - x0) as usize) * 4;
                for c in 0..3 {
                    out[o + c] = (out[o + c] & !1) | ((r >> c) & 1) as u8;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn readback_is_idempotent() {
        let (w, h) = (8u32, 8u32);
        let mut canvas = vec![255u8; (w * h * 4) as usize];
        for i in 0..(w * h) as usize {
            if i % 3 == 0 {
                canvas[i * 4..i * 4 + 3].copy_from_slice(&[(i * 7) as u8, 40, 200]);
            }
        }
        let n = CanvasNoise::new([9; 16]);
        let mut once = canvas.clone();
        n.apply(&canvas, w, h, 0, 0, w, &mut once);
        assert_ne!(once, canvas);
        assert!(
            once.iter()
                .zip(&canvas)
                .all(|(a, b)| (i16::from(*a) - i16::from(*b)).abs() <= 1)
        );
        let mut twice = once.clone();
        n.apply(&once, w, h, 0, 0, w, &mut twice);
        assert_eq!(twice, once);
    }

    #[test]
    fn solid_regions_are_exact() {
        let (w, h) = (4u32, 4u32);
        let canvas: Vec<u8> = [10u8, 20, 30, 255].repeat((w * h) as usize);
        let mut out = canvas.clone();
        CanvasNoise::new([7; 16]).apply(&canvas, w, h, 0, 0, w, &mut out);
        assert_eq!(out, canvas);
    }

    #[test]
    fn edges_are_noised_per_seed_and_subrects_agree() {
        let (w, h) = (16u32, 16u32);
        let mut canvas = vec![0u8; (w * h * 4) as usize];
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                let v = (x * 16) as u8;
                canvas[i..i + 4].copy_from_slice(&[v, 255 - v, (y * 9) as u8, 255]);
            }
        }
        let full = |seed: u8| {
            let mut out = canvas.clone();
            CanvasNoise::new([seed; 16]).apply(&canvas, w, h, 0, 0, w, &mut out);
            out
        };
        let a = full(1);
        assert_ne!(a, canvas);
        assert_eq!(a, full(1));
        assert_ne!(a, full(2));
        // A sub-rect read matches the same pixels of a full read.
        let (sx, sy, sw, sh) = (3i64, 5i64, 6u32, 4usize);
        let mut sub = Vec::new();
        for y in 0..sh {
            let i = ((sy as usize + y) * w as usize + sx as usize) * 4;
            sub.extend_from_slice(&canvas[i..i + sw as usize * 4]);
        }
        CanvasNoise::new([1; 16]).apply(&canvas, w, h, sx, sy, sw, &mut sub);
        for y in 0..sh {
            let i = ((sy as usize + y) * w as usize + sx as usize) * 4;
            assert_eq!(
                &sub[y * sw as usize * 4..(y + 1) * sw as usize * 4],
                &a[i..i + sw as usize * 4]
            );
        }
    }
}
