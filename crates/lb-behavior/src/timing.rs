// timing.rs — Navigation timing distributions (vendored from stealth-browser)
//
// ## Why timing matters for bot detection
//
// Timing is the most scrutinised behavioural channel.  ML-based detectors
// (Cloudflare Bot Management, Kasada, DataDome) build per-session feature
// vectors that include:
//
//   • Time-to-first-interaction (TTFI): how long after DOMContentLoaded does
//     the first input event occur?  Bots typically interact within 0–100 ms;
//     humans take 0.5–5 s (page rendering + reading the page + deciding what
//     to click).
//
//   • Action inter-arrival times: distribution of gaps between consecutive
//     actions.  Uniform distributions are a bot signal; human gaps follow
//     log-normal or Weibull distributions.
//
//   • Page dwell time: how long does a session spend on each page?  Very
//     short dwell (< 2 s) with many actions is a strong bot signal.
//
//   • Reading time: proportional to visible text length — detectors
//     compare action timing against an expected reading time derived from
//     average WPM for the locale.
//
// ## Distribution choices
//
// • **Log-normal** for TTFI and dwell: both quantities have a hard lower
//   bound (> 0) and a long tail (some users take minutes).  Log-normal is
//   the standard model for positive-valued human RT data.
//
// • **Uniform** for think time: the range is short (< 2 s) and there is no
//   strong empirical evidence for a specific shape within this range.

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{clamp, normal_sample, uniform};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Per-session navigation timing parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TimingProfile {
    /// (min_ms, max_ms) for the initial delay after page load before any
    /// interaction.  The actual sample uses a log-normal distribution fitted
    /// to this range (see `sample_first_interaction`).
    pub first_interaction_ms: (u64, u64),
    /// (min_ms, max_ms) for page dwell time.  Log-normal.
    pub dwell_ms: (u64, u64),
    /// Reading speed in words per minute — used to derive `reading_time_ms`.
    pub reading_speed_wpm: f64,
    /// (min_ms, max_ms) for the think pause before each individual action.
    pub action_think_ms: (u64, u64),
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Sample from a log-normal distribution whose range spans approximately
/// [range_min, range_max].
///
/// We fit μ and σ of the underlying normal by matching the 5th and 95th
/// percentiles to range_min and range_max respectively:
///
///   ln(range_min) ≈ μ - 1.645σ
///   ln(range_max) ≈ μ + 1.645σ
///
/// Solving:
///   σ = (ln(range_max) - ln(range_min)) / (2 × 1.645)
///   μ = (ln(range_max) + ln(range_min)) / 2
fn sample_log_normal(rng: &mut SmallRng, range_min: u64, range_max: u64) -> u64 {
    let lo = (range_min as f64).max(1.0).ln();
    let hi = (range_max as f64).max(1.0).ln();

    let mu = (lo + hi) / 2.0;
    let sigma = (hi - lo) / (2.0 * 1.645);

    // Draw Z ~ N(0, 1) and transform: X = exp(μ + σZ).
    let z = normal_sample(rng, 0.0, 1.0);
    let sample = (mu + sigma * z).exp();

    // Clamp to the given range to avoid extreme outliers.
    clamp(sample, range_min as f64, range_max as f64 * 1.5) as u64
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Sample the delay between page load completion and the first user action.
///
/// Uses a log-normal distribution to produce the characteristic right-skewed
/// histogram seen in real navigation data: most sessions interact within
/// 1–3 s, but a meaningful fraction take 5–30 s (reading the page first).
///
/// # Bot detection context
///
/// Values < 300 ms are extremely suspicious — no human can read a page and
/// decide what to do that quickly.  Values > 60 s are also suspicious unless
/// the page has a lot of text.  The replay layer (lb-cdp) should validate that
/// the generated value is plausible for the page's content length.
pub fn sample_first_interaction(profile: &TimingProfile, rng: &mut SmallRng) -> u64 {
    let (min, max) = profile.first_interaction_ms;
    sample_log_normal(rng, min, max)
}

/// Sample the total dwell time on a page (time from page load to navigation).
///
/// Dwell time is highly variable and correlated with page content.  The
/// log-normal model captures the heavy tail: most sessions are short but a
/// few are very long.
///
/// The caller may wish to add `reading_time_ms(content_length, profile)` to
/// the sampled dwell to ensure the session "has time to read" the page.
pub fn sample_dwell(profile: &TimingProfile, rng: &mut SmallRng) -> u64 {
    let (min, max) = profile.dwell_ms;
    sample_log_normal(rng, min, max)
}

/// Sample the think-time pause before an individual action (click, type, etc.).
///
/// This models the cognitive preparation time: the user decides where to
/// click, moves their eyes to the target, and initiates the motor action.
/// In reaction-time literature this is sometimes called the "decision latency"
/// component of Fitts' Law.
///
/// We use a uniform distribution here because the range is narrow (< 2 s in
/// most profiles) and the empirical distribution within this range is roughly
/// flat for generic web interactions.
///
/// For more specific interactions (e.g. filling a known form field vs.
/// discovering where a button is), the caller should apply a multiplier to
/// the sampled value.
pub fn sample_think_time(profile: &TimingProfile, rng: &mut SmallRng) -> u64 {
    let (min, max) = profile.action_think_ms;
    uniform(rng, min as f64, max as f64) as u64
}

/// Estimate the time (in ms) a user of the given profile would spend reading
/// `text_length` characters of visible page content.
///
/// Formula:
///   words = text_length / 5        (standard average word length)
///   minutes = words / reading_speed_wpm
///   milliseconds = minutes × 60_000
///
/// # Usage
///
/// Add this to `sample_dwell` to get a lower bound on realistic dwell:
///
/// ```ignore
/// let min_dwell = reading_time_ms(page_text.len(), &profile.timing);
/// let dwell = sample_dwell(&profile.timing, rng).max(min_dwell);
/// ```
///
/// # Bot detection context
///
/// Pages with 500+ words of content require ~2 minutes at 250 WPM to read.
/// A session that dwells for only 5 s on such a page and then fills in a
/// form is suspicious.  The engine does not enforce this — that is the
/// caller's responsibility — but this function provides the calculation.
pub fn reading_time_ms(text_length: usize, profile: &TimingProfile) -> u64 {
    if text_length == 0 || profile.reading_speed_wpm <= 0.0 {
        return 0;
    }

    let words = text_length as f64 / 5.0;
    let minutes = words / profile.reading_speed_wpm;
    let ms = minutes * 60_000.0;

    ms as u64
}

/// Compute a realistic session timeline for navigating to and interacting
/// with a page.
///
/// Returns a tuple of:
///   `(first_interaction_ms, dwell_ms, think_before_action_ms)`
///
/// This is a convenience wrapper for the replay layer (lb-cdp) that needs all
/// three values at once for a given navigation step.
pub fn sample_navigation_timing(profile: &TimingProfile, rng: &mut SmallRng) -> (u64, u64, u64) {
    let first = sample_first_interaction(profile, rng);
    let dwell = sample_dwell(profile, rng);
    let think = sample_think_time(profile, rng);
    (first, dwell, think)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    fn casual_timing() -> TimingProfile {
        BehaviorProfile::casual_user().timing
    }

    #[test]
    fn first_interaction_in_range() {
        let profile = casual_timing();
        let mut rng = seeded_rng(30);
        for _ in 0..1000 {
            let t = sample_first_interaction(&profile, &mut rng);
            // Allow 1.5x the max for log-normal tail.
            assert!(
                t >= profile.first_interaction_ms.0 && t <= profile.first_interaction_ms.1 * 2,
                "out of range: {}",
                t
            );
        }
    }

    #[test]
    fn dwell_in_range() {
        let profile = casual_timing();
        let mut rng = seeded_rng(31);
        for _ in 0..1000 {
            let t = sample_dwell(&profile, &mut rng);
            assert!(
                t >= profile.dwell_ms.0 && t <= profile.dwell_ms.1 * 2,
                "out of range: {}",
                t
            );
        }
    }

    #[test]
    fn think_time_in_range() {
        let profile = casual_timing();
        let mut rng = seeded_rng(32);
        for _ in 0..1000 {
            let t = sample_think_time(&profile, &mut rng);
            assert!(
                t >= profile.action_think_ms.0 && t <= profile.action_think_ms.1,
                "out of range: {}",
                t
            );
        }
    }

    #[test]
    fn reading_time_scales_with_length() {
        let profile = casual_timing();
        let short = reading_time_ms(100, &profile);
        let long = reading_time_ms(1000, &profile);
        assert!(long > short, "longer text should take longer to read");
    }

    #[test]
    fn reading_time_zero_text() {
        let profile = casual_timing();
        assert_eq!(reading_time_ms(0, &profile), 0);
    }

    #[test]
    fn navigation_timing_tuple_is_positive() {
        let profile = casual_timing();
        let mut rng = seeded_rng(33);
        let (fi, dw, th) = sample_navigation_timing(&profile, &mut rng);
        assert!(fi > 0);
        assert!(dw > 0);
        assert!(th > 0);
    }

    #[test]
    fn log_normal_distribution_has_right_skew() {
        // The mean of a log-normal distribution is greater than its median.
        // Sample many values and verify mean > median.
        let profile = casual_timing();
        let mut rng = seeded_rng(34);
        let mut samples: Vec<u64> = (0..10_000)
            .map(|_| sample_first_interaction(&profile, &mut rng))
            .collect();
        samples.sort_unstable();
        let median = samples[samples.len() / 2] as f64;
        let mean = samples.iter().sum::<u64>() as f64 / samples.len() as f64;
        assert!(
            mean > median,
            "log-normal should have mean > median, got mean={:.1} median={:.1}",
            mean,
            median
        );
    }
}
