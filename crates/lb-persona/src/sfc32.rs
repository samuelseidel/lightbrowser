//! SFC32 PRNG, bit-identical to `gp_sfc32_*` in stealth-browser's
//! `geckophantom_config/include/geckophantom_config.h`, so one persona seed
//! produces the same canvas/WebGL/audio noise in both browsers.

/// Small Fast Counting PRNG (32-bit). Same seed, same sequence.
#[derive(Debug, Clone)]
pub struct Sfc32 {
    a: u32,
    b: u32,
    c: u32,
    d: u32,
}

impl Sfc32 {
    /// Seeds from 16 bytes (big-endian words) and runs 12 warm-up rounds.
    pub fn new(seed: [u8; 16]) -> Self {
        let w = |i: usize| u32::from_be_bytes([seed[i], seed[i + 1], seed[i + 2], seed[i + 3]]);
        let mut s = Self {
            a: w(0),
            b: w(4),
            c: w(8),
            d: w(12),
        };
        for _ in 0..12 {
            s.next_u32();
        }
        s
    }

    pub fn next_u32(&mut self) -> u32 {
        let t = self.a.wrapping_add(self.b).wrapping_add(self.d);
        self.d = self.d.wrapping_add(1);
        self.a = self.b ^ (self.b >> 9);
        self.b = self.c.wrapping_add(self.c << 3);
        self.c = self.c.rotate_left(21).wrapping_add(t);
        t
    }

    /// Integer in `[-amplitude, amplitude]` (matches `gp_noise`).
    pub fn noise(&mut self, amplitude: i32) -> i32 {
        if amplitude <= 0 {
            return 0;
        }
        let range = (2 * amplitude + 1) as u32;
        (self.next_u32() % range) as i32 - amplitude
    }

    /// Float in `[-amplitude, amplitude)` (matches `gp_noise_double`).
    pub fn noise_f64(&mut self, amplitude: f64) -> f64 {
        let unit = f64::from(self.next_u32()) / 4_294_967_296.0;
        (unit * 2.0 - 1.0) * amplitude
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference values produced by compiling the C header with gcc,
    /// seed[i] = i * 17 + 3.
    #[test]
    fn matches_c_reference() {
        let mut seed = [0u8; 16];
        for (i, b) in seed.iter_mut().enumerate() {
            *b = (i * 17 + 3) as u8;
        }
        let mut r = Sfc32::new(seed);
        let got: Vec<u32> = (0..4).map(|_| r.next_u32()).collect();
        assert_eq!(got, [2969259439, 2901795460, 2852680735, 733612746]);
        let noise: Vec<i32> = (0..4).map(|_| r.noise(1)).collect();
        assert_eq!(noise, [0, 1, 0, 1]);
    }

    #[test]
    fn noise_stays_in_range() {
        let mut r = Sfc32::new([7; 16]);
        for _ in 0..10_000 {
            assert!((-3..=3).contains(&r.noise(3)));
            assert!(r.noise_f64(0.0001).abs() <= 0.0001);
        }
    }
}
