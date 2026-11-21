// rng.rs — Seeded PRNG utilities (vendored from stealth-browser)
//
// All randomness in the engine flows through a seeded SmallRng so that
// sessions are fully reproducible for debugging and regression testing.
// Using a seeded RNG also lets the replay layer (lb-cdp) record the seed
// alongside the generated event stream and replay it exactly.
//
// SmallRng is chosen over StdRng because:
//   • It is not cryptographically secure, but we do not need security here.
//   • It is significantly faster, which matters when generating thousands of
//     mouse-movement samples per path.
//   • It is still high-quality enough to pass statistical tests that bot
//     detectors might run on timing distributions.

use rand::rngs::SmallRng;
use rand::{Rng, SeedableRng};

/// Create a fresh SmallRng seeded from a u64.
///
/// The same seed always produces the same sequence of events, making
/// every "human session" deterministic and auditable.
pub fn seeded_rng(seed: u64) -> SmallRng {
    SmallRng::seed_from_u64(seed)
}

/// Sample from a normal (Gaussian) distribution using the Box-Muller transform.
///
/// Box-Muller converts two uniform samples U1, U2 ∈ (0, 1] into a pair of
/// independent standard-normal samples.  We only return one of them here.
///
/// Human timing data (inter-keystroke intervals, mouse-speed variability,
/// dwell times) are well-approximated by normal distributions — the Central
/// Limit Theorem applies because each measurement is the sum of many small
/// independent neuromotor delays.
///
/// # Arguments
/// * `rng`  — mutable reference to the caller's RNG state
/// * `mean` — desired distribution mean
/// * `std`  — desired standard deviation (must be > 0 for non-trivial output)
pub fn normal_sample(rng: &mut SmallRng, mean: f64, std: f64) -> f64 {
    // Draw two independent uniform samples from (0, 1].
    // We avoid exactly 0 because ln(0) = -∞.
    let u1: f64 = {
        let mut v = rng.random::<f64>();
        while v == 0.0 {
            v = rng.random::<f64>();
        }
        v
    };
    let u2: f64 = rng.random::<f64>();

    // Box-Muller formula: Z0 = sqrt(-2 ln U1) * cos(2π U2)
    let z0 = (-2.0_f64 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();

    mean + std * z0
}

/// Sample a uniform value in [min, max).
///
/// Used when we want equal probability across a range — for example, when
/// choosing a random control-point offset for Bézier curves, or picking a
/// random scroll step size within a known human range.
pub fn uniform(rng: &mut SmallRng, min: f64, max: f64) -> f64 {
    if min >= max {
        return min;
    }
    min + rng.random::<f64>() * (max - min)
}

/// Sample a log-normal value with the given median and log-space standard
/// deviation `sigma`: `median · exp(N(0, sigma))`.
///
/// Human response and movement times are positive and right-skewed; the
/// log-normal is the usual model for them (e.g. reaction times, key hold
/// times, inter-key intervals).
pub fn log_normal(rng: &mut SmallRng, median: f64, sigma: f64) -> f64 {
    median * normal_sample(rng, 0.0, sigma).exp()
}

/// `true` with probability `p`.
pub fn chance(rng: &mut SmallRng, p: f64) -> bool {
    rng.random::<f64>() < p
}

/// SplitMix64 finaliser: derives an independent per-action seed from a
/// session seed and an action index, so one root seed reproduces a whole
/// session while no two actions share an event stream.
pub fn mix_seed(seed: u64, index: u64) -> u64 {
    let mut z = seed.wrapping_add(index.wrapping_add(1).wrapping_mul(0x9e37_79b9_7f4a_7c15));
    z = (z ^ (z >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
    z ^ (z >> 31)
}

/// FNV-1a over `bytes`: a stable 64-bit seed from a string (for example a
/// persona id), the same on every platform and run.
pub fn seed_from_bytes(bytes: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in bytes {
        h ^= u64::from(b);
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

/// Clamp `val` into the closed interval [min, max].
///
/// Keeps sampled values from producing degenerate geometry (e.g. negative
/// timestamps or mouse coordinates outside the viewport).
pub fn clamp(val: f64, min: f64, max: f64) -> f64 {
    val.max(min).min(max)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seeded_rng_is_deterministic() {
        let mut a = seeded_rng(42);
        let mut b = seeded_rng(42);
        for _ in 0..100 {
            assert_eq!(
                normal_sample(&mut a, 0.0, 1.0),
                normal_sample(&mut b, 0.0, 1.0)
            );
        }
    }

    #[test]
    fn normal_sample_rough_statistics() {
        let mut rng = seeded_rng(1);
        let samples: Vec<f64> = (0..10_000)
            .map(|_| normal_sample(&mut rng, 0.0, 1.0))
            .collect();
        let mean = samples.iter().sum::<f64>() / samples.len() as f64;
        let variance =
            samples.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / samples.len() as f64;
        // Mean should be near 0, std near 1
        assert!(mean.abs() < 0.05, "mean too far from 0: {}", mean);
        assert!(
            (variance.sqrt() - 1.0).abs() < 0.05,
            "std too far from 1: {}",
            variance.sqrt()
        );
    }

    #[test]
    fn uniform_stays_in_range() {
        let mut rng = seeded_rng(99);
        for _ in 0..1000 {
            let v = uniform(&mut rng, 10.0, 20.0);
            assert!((10.0..20.0).contains(&v));
        }
    }

    #[test]
    fn log_normal_median() {
        let mut rng = seeded_rng(5);
        let mut v: Vec<f64> = (0..10_001)
            .map(|_| log_normal(&mut rng, 100.0, 0.3))
            .collect();
        v.sort_by(f64::total_cmp);
        assert!((v[5000] - 100.0).abs() < 3.0, "median {}", v[5000]);
        assert!(v[0] > 0.0);
    }

    #[test]
    fn mix_seed_spreads() {
        assert_ne!(mix_seed(1, 0), mix_seed(1, 1));
        assert_ne!(mix_seed(1, 0), mix_seed(2, 0));
        assert_eq!(mix_seed(7, 3), mix_seed(7, 3));
        assert_eq!(seed_from_bytes(b"a"), seed_from_bytes(b"a"));
        assert_ne!(seed_from_bytes(b"a"), seed_from_bytes(b"b"));
    }

    #[test]
    fn clamp_works() {
        assert_eq!(clamp(-1.0, 0.0, 1.0), 0.0);
        assert_eq!(clamp(2.0, 0.0, 1.0), 1.0);
        assert_eq!(clamp(0.5, 0.0, 1.0), 0.5);
    }
}
