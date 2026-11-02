// keyboard.rs — Realistic keystroke timing simulation (vendored from stealth-browser)
//
// ## Human Typing Model
//
// Real typing timing is NOT uniform.  Studies (Arif & Stuerzlinger 2009;
// Dhakal et al. 2018 "Observations on Typing from 136 Million Keystrokes")
// show several key properties we must reproduce:
//
//   1. **High variance**: Even at constant WPM, inter-key intervals (IKI)
//      have a coefficient of variation (σ/μ) of about 0.3.  We model IKI as
//      N(μ, 0.3μ) — a standard deviation equal to 30 % of the mean.
//
//   2. **Bigram effects**: Certain key pairs (bigrams) are consistently faster
//      or slower than the mean, determined by hand-position geometry and
//      cognitive priming.  "th" (right-hand T followed by right-hand H) is
//      one of the fastest bigrams; "zx" is one of the slowest.  We apply
//      multiplicative corrections for known bigrams.
//
//   3. **Errors and corrections**: Real typists make ~4 % errors per
//      character (Dhakal 2018), catch them within 0.5–2 s, and backspace.
//      The error–detection–correction cycle is a strong human signal that
//      is absent from programmatic text insertion.
//
//   4. **Key-hold duration**: Each key has a press duration (KeyDown to
//      KeyUp) separate from the inter-key interval.  Mean hold is ~80 ms
//      with ±40 ms variation.  Bots often emit zero-duration keypresses.
//
//   5. **Burst–pause rhythm**: Fluent typists produce bursts of rapid
//      keystrokes followed by brief pauses (word boundaries, thinking).
//      We add an extra pause at word boundaries (space key) to capture this.

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{clamp, normal_sample, uniform};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Browser-level keyboard event type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyEventType {
    Down,
    Up,
    /// Synthetic KeyPress (character insertion) — fires between Down and Up.
    Press,
}

/// A single synthesised keyboard event.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeystrokeEvent {
    /// Key name (e.g. "a", "Backspace", "Shift") or character.
    pub key: String,
    pub event_type: KeyEventType,
    /// Milliseconds since the start of the typing sequence.
    pub timestamp_ms: u64,
}

/// Per-session keyboard behaviour parameters.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyboardProfile {
    /// Mean words per minute.
    pub wpm_mean: f64,
    /// Standard deviation of WPM across a session.
    pub wpm_std: f64,
    /// Probability of a typo per character (0.0–1.0).
    pub error_rate: f64,
    /// (min_ms, max_ms): delay between noticing an error and pressing
    /// Backspace.  Uniform over this range.
    pub correction_delay_ms: (u64, u64),
    /// If true, apply bigram-specific timing corrections.
    pub bigram_variation: bool,
}

// ---------------------------------------------------------------------------
// Bigram timing database
// ---------------------------------------------------------------------------

/// Returns a timing multiplier for the bigram formed by `prev` → `curr`.
///
/// Values < 1.0 mean the transition is faster than average (e.g. strong
/// same-hand rolls); values > 1.0 mean it is slower (e.g. awkward reaches
/// or alternating-hand transitions that break rhythm).
///
/// Data is derived from the literature (Arif & Stuerzlinger 2009) and the
/// open-source 136M-keystroke dataset (Dhakal et al. 2018).
fn bigram_multiplier(prev: char, curr: char) -> f64 {
    // Represent the bigram as a lowercase pair for easy matching.
    let p = prev.to_ascii_lowercase();
    let c = curr.to_ascii_lowercase();

    // Fast bigrams (strong same-hand rolls, common in English):
    // These are memorised as motor chunks, so the second key is often
    // pressed before the first is fully released (negative IKI in expert
    // typists).  We cap the speedup at 0.45× (less than half the mean IKI).
    let fast: &[(&str, f64)] = &[
        ("th", 0.50),
        ("he", 0.52),
        ("in", 0.55),
        ("er", 0.53),
        ("an", 0.56),
        ("re", 0.54),
        ("on", 0.57),
        ("en", 0.58),
        ("at", 0.55),
        ("es", 0.56),
        ("st", 0.54),
        ("nt", 0.57),
        ("ou", 0.58),
        ("ea", 0.55),
        ("nd", 0.56),
        ("to", 0.60),
        ("or", 0.60),
        ("it", 0.58),
        ("is", 0.59),
        ("ng", 0.52),
        ("ha", 0.57),
        ("as", 0.58),
        ("hi", 0.57),
        ("ed", 0.57),
    ];

    // Slow bigrams (awkward same-finger or long reaches):
    let slow: &[(&str, f64)] = &[
        ("zx", 2.20),
        ("qp", 2.10),
        ("xz", 2.20),
        ("pq", 2.10),
        ("zy", 1.90),
        ("qo", 1.80),
        ("bv", 1.70),
        ("vb", 1.70),
        ("nm", 1.50),
        ("mn", 1.50),
        ("jk", 1.40),
        ("kj", 1.40),
    ];

    let key = format!("{}{}", p, c);

    for (bigram, mult) in fast {
        if *bigram == key.as_str() {
            return *mult;
        }
    }
    for (bigram, mult) in slow {
        if *bigram == key.as_str() {
            return *mult;
        }
    }

    // No specific data: return 1.0 (use baseline IKI).
    1.0
}

/// Return a plausible adjacent key for a typo (qwerty layout proximity).
///
/// Bot detectors that analyse text-edit events look for realistic error
/// characters.  Replacing 'e' with 'r' is realistic; replacing 'e' with 'z'
/// is suspicious.
fn adjacent_key(c: char) -> char {
    // Map each key to its nearest neighbours on a standard QWERTY layout.
    let adjacency: &[(char, &str)] = &[
        ('q', "wa"),
        ('w', "qeasd"),
        ('e', "wrsdf"),
        ('r', "etdfg"),
        ('t', "ryfgh"),
        ('y', "tughj"),
        ('u', "yihjk"),
        ('i', "uojkl"),
        ('o', "ipkl"),
        ('p', "ol"),
        ('a', "qwsz"),
        ('s', "awedxz"),
        ('d', "serfcx"),
        ('f', "drtgvc"),
        ('g', "ftyhbv"),
        ('h', "gyujnb"),
        ('j', "huikm"),
        ('k', "jiol"),
        ('l', "kop"),
        ('z', "asx"),
        ('x', "zsdc"),
        ('c', "xdfv"),
        ('v', "cfgb"),
        ('b', "vghn"),
        ('n', "bhjm"),
        ('m', "njk"),
        (' ', " "), // space → space (no meaningful adjacent key)
    ];

    let lc = c.to_ascii_lowercase();
    for (key, neighbours) in adjacency {
        if *key == lc {
            if neighbours.is_empty() {
                return c;
            }
            // Return the first neighbour as a deterministic choice.
            // (The caller's RNG already decided we're making an error.)
            return neighbours.chars().next().unwrap_or(c);
        }
    }
    c // Unknown character — return unchanged.
}

// ---------------------------------------------------------------------------
// Internal event builder
// ---------------------------------------------------------------------------

/// Append Down → Press → Up events for a single `key` at the given cursor.
///
/// Returns the updated cursor position (timestamp_ms after the KeyUp).
fn push_key_events(
    events: &mut Vec<KeystrokeEvent>,
    key: &str,
    cursor_ms: u64,
    hold_ms: u64,
) -> u64 {
    events.push(KeystrokeEvent {
        key: key.to_string(),
        event_type: KeyEventType::Down,
        timestamp_ms: cursor_ms,
    });
    // KeyPress fires immediately after KeyDown in browser event model.
    events.push(KeystrokeEvent {
        key: key.to_string(),
        event_type: KeyEventType::Press,
        timestamp_ms: cursor_ms,
    });
    events.push(KeystrokeEvent {
        key: key.to_string(),
        event_type: KeyEventType::Up,
        timestamp_ms: cursor_ms + hold_ms,
    });
    cursor_ms + hold_ms
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a realistic sequence of keyboard events to type `text`.
///
/// # Algorithm
///
/// 1. Derive the base inter-key interval (IKI) from the session's WPM.
///    The standard approximation is: 1 word = 5 characters, so:
///    `IKI_ms = 60_000 / (WPM × 5)`.
///    We further sample WPM from N(wpm_mean, wpm_std) to vary it across
///    calls (different paragraphs have slightly different rates).
///
/// 2. For each character:
///    a. Sample the actual IKI from N(base_IKI, base_IKI × 0.3).
///    b. Apply a bigram multiplier if `bigram_variation` is enabled.
///    c. Apply a word-boundary pause if the previous character was a space.
///    d. With probability `error_rate`, inject a typo: type an adjacent key,
///    wait `correction_delay_ms`, press Backspace, then type the correct
///    key.
///    e. Emit Down → Press → Up for each key with a realistic hold duration.
///
/// 3. Return the flat, time-ordered `Vec<KeystrokeEvent>`.
pub fn generate_typing_events(
    text: &str,
    profile: &KeyboardProfile,
    rng: &mut SmallRng,
) -> Vec<KeystrokeEvent> {
    if text.is_empty() {
        return Vec::new();
    }

    // Sample an effective WPM for this typing chunk.
    let effective_wpm = clamp(
        normal_sample(rng, profile.wpm_mean, profile.wpm_std),
        5.0,   // absolute minimum — pathologically slow
        250.0, // absolute maximum — world-record speed
    );

    // Base inter-key interval in milliseconds.
    // (60_000 ms/min) / (WPM × 5 chars/word) = ms per character.
    let base_iki_ms = 60_000.0 / (effective_wpm * 5.0);

    let mut events: Vec<KeystrokeEvent> = Vec::new();
    let mut cursor_ms: u64 = 0;
    let mut prev_char: Option<char> = None;

    let chars: Vec<char> = text.chars().collect();

    for (idx, &ch) in chars.iter().enumerate() {
        // ---- 1. Inter-key interval for this character -------------------

        // High-variance Gaussian IKI (CV ≈ 0.3 as per Dhakal 2018).
        let mut iki = normal_sample(rng, base_iki_ms, base_iki_ms * 0.3);
        iki = iki.max(20.0); // physical lower bound: key travel + neural latency

        // Apply bigram correction.
        if profile.bigram_variation
            && let Some(p) = prev_char
        {
            let mult = bigram_multiplier(p, ch);
            iki *= mult;
        }

        // Word-boundary pause: after typing a space, typists often pause
        // briefly before the next word (cognitive planning of the next word).
        // This pause is absent within words.  We add 50–200 ms extra.
        if prev_char == Some(' ') {
            iki += uniform(rng, 50.0, 200.0);
        }

        // Advance the cursor by the inter-key interval.
        // For the first character, IKI is the "startup" delay before typing.
        cursor_ms += iki as u64;

        // ---- 2. Key-hold duration ----------------------------------------
        // Hold time is the duration from KeyDown to KeyUp.
        // Mean ~80 ms, std ~40 ms, clamped to [15, 200] ms.
        // Short holds for common keys (home row), longer for awkward reaches.
        let hold_ms = clamp(normal_sample(rng, 80.0, 40.0), 15.0, 200.0) as u64;

        // ---- 3. Typo injection -------------------------------------------
        // With probability `error_rate`, type an adjacent wrong key first.
        let make_error = uniform(rng, 0.0, 1.0) < profile.error_rate
            && ch != ' '    // spaces are rarely mistyped as a character
            && idx < chars.len().saturating_sub(1); // don't error on last char

        if make_error {
            // Type the wrong key.
            let wrong_ch = adjacent_key(ch);
            let wrong_key = wrong_ch.to_string();
            let wrong_hold = clamp(normal_sample(rng, 80.0, 40.0), 15.0, 200.0) as u64;
            cursor_ms = push_key_events(&mut events, &wrong_key, cursor_ms, wrong_hold);

            // Detection and correction delay: time before user notices the
            // error and reaches for Backspace.  Uniform over the profile range.
            let (corr_min, corr_max) = profile.correction_delay_ms;
            let correction_delay = uniform(rng, corr_min as f64, corr_max as f64) as u64;
            cursor_ms += correction_delay;

            // Press Backspace.
            let bs_hold = clamp(normal_sample(rng, 80.0, 30.0), 15.0, 150.0) as u64;
            cursor_ms = push_key_events(&mut events, "Backspace", cursor_ms, bs_hold);

            // Brief pause after correction before re-typing.
            cursor_ms += uniform(rng, 30.0, 100.0) as u64;
        }

        // ---- 4. Type the correct key ------------------------------------
        let key = match ch {
            '\n' => "Enter".to_string(),
            '\t' => "Tab".to_string(),
            _ => ch.to_string(),
        };

        cursor_ms = push_key_events(&mut events, &key, cursor_ms, hold_ms);

        prev_char = Some(ch);
    }

    events
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    fn default_profile() -> KeyboardProfile {
        BehaviorProfile::casual_user().keyboard
    }

    #[test]
    fn events_are_monotonic() {
        let profile = default_profile();
        let mut rng = seeded_rng(10);
        let events = generate_typing_events("Hello, world!", &profile, &mut rng);
        for w in events.windows(2) {
            assert!(
                w[1].timestamp_ms >= w[0].timestamp_ms,
                "non-monotonic: {} > {}",
                w[0].timestamp_ms,
                w[1].timestamp_ms
            );
        }
    }

    #[test]
    fn events_contain_expected_characters() {
        let text = "hi";
        let profile = default_profile();
        let mut rng = seeded_rng(11);
        let events = generate_typing_events(text, &profile, &mut rng);

        // At minimum, 'h' and 'i' must appear as KeyDown events.
        let down_keys: Vec<&str> = events
            .iter()
            .filter(|e| e.event_type == KeyEventType::Down)
            .map(|e| e.key.as_str())
            .collect();

        assert!(down_keys.contains(&"h"), "missing 'h': {:?}", down_keys);
        assert!(down_keys.contains(&"i"), "missing 'i': {:?}", down_keys);
    }

    #[test]
    fn backspace_appears_on_error_injection() {
        // Use a profile with 100 % error rate to guarantee a Backspace.
        let mut profile = default_profile();
        profile.error_rate = 1.0;
        profile.correction_delay_ms = (50, 100);
        let mut rng = seeded_rng(12);
        let events = generate_typing_events("abc", &profile, &mut rng);
        let has_backspace = events.iter().any(|e| e.key == "Backspace");
        assert!(
            has_backspace,
            "expected Backspace events with 100% error rate"
        );
    }

    #[test]
    fn empty_text_returns_empty() {
        let profile = default_profile();
        let mut rng = seeded_rng(13);
        let events = generate_typing_events("", &profile, &mut rng);
        assert!(events.is_empty());
    }

    #[test]
    fn bigram_multiplier_fast() {
        // "th" should be less than 1.0 (faster than mean).
        assert!(bigram_multiplier('t', 'h') < 1.0);
    }

    #[test]
    fn bigram_multiplier_slow() {
        // "zx" should be greater than 1.0 (slower than mean).
        assert!(bigram_multiplier('z', 'x') > 1.0);
    }
}
