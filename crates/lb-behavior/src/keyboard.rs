// keyboard.rs: keystroke timing.
//
// Rewritten in LightBrowser (M5); the vendored generator emitted strictly
// sequential keys (no overlap), no Shift key and a Gaussian IKI. The model
// here, and where each piece comes from:
//
// - Inter-key interval (IKI, key-down to key-down): log-normal (right
//   skewed, as in every large keystroke dataset) with sigma `iki_sigma`
//   (~0.35-0.45, a coefficient of variation of 0.4-0.5), median set so
//   the mean matches the session's WPM (1 word = 5 characters). Dhakal et
//   al. 2018 ("Observations on Typing from 136 Million Keystrokes"): mean
//   IKI 239 ms at 52 WPM.
// - Bigram effects: common same-hand rolls and alternating-hand pairs are
//   faster, awkward reaches slower (multipliers below; Salthouse 1986,
//   Dhakal 2018). Word-initial keys are slower (x1.25), and with
//   `hesitation_rate` a word starts after an extra pause (log-normal,
//   median 400 ms) for planning the next word.
// - Key hold (dwell): log-normal around a per-session median near
//   `hold_ms` (Dhakal 2018: mean key press duration ~116 ms) with sigma
//   `hold_sigma` (within-person CV of holds is about 0.2-0.3 in the CMU
//   keystroke benchmark, Killourhy & Maxion 2009). Keys are struck by
//   different fingers, so each key also has its own typical hold: a
//   factor log-normal with sigma `hold_key_spread`, fixed per session by
//   `key_seed` (the thumb on the space bar holds about 15 % longer). Because IKI is down-to-down and holds are independent,
//   fast typists press the next key before releasing the previous one
//   (rollover), as real typists do; the same key cannot be pressed again
//   before it was released.
// - Shift: pressed before the first shifted character of a run (lead
//   log-normal, median 65 ms) and released after the run's last key-up
//   (log-normal, median 30 ms), before the next unshifted key goes down.
//   Shifted keystrokes take longer (extra ~80 ms).
// - Errors: with `error_rate` per letter a neighbouring QWERTY key is hit
//   instead; the typist notices it after 0, 1 or 2 more characters (60 /
//   30 / 10 %), waits `correction_delay_ms`, presses Backspace that many
//   times (median 160 ms apart) and retypes.

use rand::rngs::SmallRng;
use serde::{Deserialize, Serialize};

use crate::rng::{
    chance, clamp, log_normal, mix_seed, normal_sample, seed_from_bytes, seeded_rng, uniform,
};

// ---------------------------------------------------------------------------
// Data types
// ---------------------------------------------------------------------------

/// Browser-level keyboard event type.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum KeyEventType {
    Down,
    Up,
    /// Synthetic KeyPress (character insertion): fires right after Down.
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
#[serde(default)]
pub struct KeyboardProfile {
    /// Mean words per minute.
    pub wpm_mean: f64,
    /// Standard deviation of WPM between typing bursts.
    pub wpm_std: f64,
    /// Probability of a typo per letter (0.0-1.0).
    pub error_rate: f64,
    /// (min_ms, max_ms): delay between noticing an error and pressing
    /// Backspace. Uniform over this range.
    pub correction_delay_ms: (u64, u64),
    /// If true, apply bigram-specific timing corrections.
    pub bigram_variation: bool,
    /// Log-space standard deviation of the inter-key interval.
    pub iki_sigma: f64,
    /// Median key hold time in ms.
    pub hold_ms: f64,
    /// Log-space standard deviation of the key hold time.
    pub hold_sigma: f64,
    /// Probability that a word starts after a planning pause.
    pub hesitation_rate: f64,
    /// Log-space spread of the per-key hold factors.
    pub hold_key_spread: f64,
    /// Seed of the per-key hold factors (one per session; lb-page sets it
    /// from the persona).
    pub key_seed: u64,
}

impl Default for KeyboardProfile {
    fn default() -> Self {
        crate::BehaviorProfile::casual_user().keyboard
    }
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

/// Neighbours of a key on a US QWERTY layout (for realistic typos).
fn neighbours(c: char) -> &'static str {
    match c.to_ascii_lowercase() {
        'q' => "wa",
        'w' => "qeasd",
        'e' => "wrsdf",
        'r' => "etdfg",
        't' => "ryfgh",
        'y' => "tughj",
        'u' => "yihjk",
        'i' => "uojkl",
        'o' => "ipkl",
        'p' => "ol",
        'a' => "qwsz",
        's' => "awedxz",
        'd' => "serfcx",
        'f' => "drtgvc",
        'g' => "ftyhbv",
        'h' => "gyujnb",
        'j' => "huikm",
        'k' => "jiol",
        'l' => "kop",
        'z' => "asx",
        'x' => "zsdc",
        'c' => "xdfv",
        'v' => "cfgb",
        'b' => "vghn",
        'n' => "bhjm",
        'm' => "njk",
        _ => "",
    }
}

/// A plausible wrong key for `c`, keeping its case; `None` for keys with
/// no letter neighbours.
fn typo_for(c: char, rng: &mut SmallRng) -> Option<char> {
    let n: Vec<char> = neighbours(c).chars().collect();
    if n.is_empty() {
        return None;
    }
    let w = n[(uniform(rng, 0.0, n.len() as f64) as usize).min(n.len() - 1)];
    Some(if c.is_ascii_uppercase() {
        w.to_ascii_uppercase()
    } else {
        w
    })
}

/// The character needs Shift on a US layout.
pub fn needs_shift(c: char) -> bool {
    c.is_ascii_uppercase() || "~!@#$%^&*()_+{}|:\"<>?".contains(c)
}

/// One planned keystroke.
struct Stroke {
    key: String,
    ch: Option<char>,
    shift: bool,
    /// Extra pause before this key (ms).
    pause: f64,
    backspace_run: bool,
}

/// The typical hold of `key` relative to the session median.
fn key_hold_factor(key: &str, profile: &KeyboardProfile) -> f64 {
    let k = key.to_lowercase();
    let mut r = seeded_rng(mix_seed(profile.key_seed, seed_from_bytes(k.as_bytes())));
    let base = if k == " " { 1.15 } else { 1.0 };
    base * log_normal(&mut r, 1.0, profile.hold_key_spread)
}

fn key_name(ch: char) -> String {
    match ch {
        '\n' => "Enter".into(),
        '\t' => "Tab".into(),
        _ => ch.to_string(),
    }
}

fn stroke(ch: char) -> Stroke {
    Stroke {
        key: key_name(ch),
        ch: Some(ch),
        shift: needs_shift(ch),
        pause: 0.0,
        backspace_run: false,
    }
}

/// The keys to press for `text`, typos and corrections included.
fn plan(text: &[char], profile: &KeyboardProfile, rng: &mut SmallRng) -> Vec<Stroke> {
    let mut out = Vec::with_capacity(text.len() + 4);
    let mut i = 0;
    let mut no_typo_at = usize::MAX;
    while i < text.len() {
        let ch = text[i];
        let typo = if i != no_typo_at
            && i + 1 < text.len()
            && ch.is_ascii_alphabetic()
            && chance(rng, profile.error_rate)
        {
            typo_for(ch, rng)
        } else {
            None
        };
        if let Some(wrong) = typo {
            out.push(stroke(wrong));
            // Notice after 0, 1 or 2 more characters.
            let r = uniform(rng, 0.0, 1.0);
            let more = if r < 0.6 {
                0
            } else if r < 0.9 {
                1
            } else {
                2
            };
            let more = more.min(text.len() - i - 1);
            for &c in &text[i + 1..=i + more] {
                out.push(stroke(c));
            }
            let (lo, hi) = profile.correction_delay_ms;
            for k in 0..=more {
                out.push(Stroke {
                    key: "Backspace".into(),
                    ch: None,
                    shift: false,
                    pause: if k == 0 {
                        uniform(rng, lo as f64, hi as f64)
                    } else {
                        0.0
                    },
                    backspace_run: k > 0,
                });
            }
            let mut s = stroke(ch);
            s.pause = uniform(rng, 30.0, 120.0);
            out.push(s);
            no_typo_at = i + 1;
            i += 1;
            continue;
        }
        let mut s = stroke(ch);
        let word_start = i > 0 && text[i - 1] == ' ' && ch != ' ';
        if word_start && chance(rng, profile.hesitation_rate) {
            s.pause = clamp(log_normal(rng, 400.0, 0.5), 120.0, 2500.0);
        }
        out.push(s);
        i += 1;
    }
    out
}

fn push(events: &mut Vec<(f64, usize, KeystrokeEvent)>, key: &str, kind: KeyEventType, t: f64) {
    let n = events.len();
    events.push((
        t,
        n,
        KeystrokeEvent {
            key: key.to_string(),
            event_type: kind,
            timestamp_ms: 0,
        },
    ));
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Generate a realistic sequence of keyboard events to type `text`.
///
/// Each key yields Down, Press (same time) and Up; Shift yields Down and
/// Up around runs of shifted characters. Events are time-ordered; keys may
/// overlap (rollover). `\n` is typed as Enter and `\t` as Tab. Replaying
/// the Down events (Backspace deleting the previous character) yields
/// exactly `text`.
pub fn generate_typing_events(
    text: &str,
    profile: &KeyboardProfile,
    rng: &mut SmallRng,
) -> Vec<KeystrokeEvent> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() {
        return Vec::new();
    }
    let wpm = clamp(
        normal_sample(rng, profile.wpm_mean, profile.wpm_std),
        10.0,
        200.0,
    );
    // Median of the log-normal so that its mean is the WPM interval.
    let sigma = profile.iki_sigma.max(0.01);
    let iki_median = 12_000.0 / wpm / (sigma * sigma / 2.0).exp();
    let hold_median = clamp(
        normal_sample(rng, profile.hold_ms, profile.hold_ms * 0.1),
        40.0,
        250.0,
    );
    let strokes = plan(&chars, profile, rng);

    let mut events: Vec<(f64, usize, KeystrokeEvent)> = Vec::new();
    let mut prev_down: Option<f64> = None;
    let mut prev_up = 0.0f64;
    let mut prev: Option<&Stroke> = None;
    let mut shift_held = false;
    let mut shifted_up = 0.0f64;
    for s in &strokes {
        // Inter-key interval.
        let mut med = iki_median;
        if profile.bigram_variation
            && let (Some(p), Some(c)) = (prev.and_then(|p| p.ch), s.ch)
        {
            med *= bigram_multiplier(p, c);
        }
        if prev.and_then(|p| p.ch) == Some(' ') && s.ch.is_some_and(|c| c != ' ') {
            med *= 1.25;
        }
        if s.backspace_run {
            med = 160.0;
        }
        let gap = clamp(log_normal(rng, med, sigma), 30.0, 3000.0);
        let mut down = match prev_down {
            None => uniform(rng, 0.0, 60.0),
            Some(t) => t + gap,
        } + s.pause;
        if s.shift && !shift_held {
            down += clamp(log_normal(rng, 80.0, 0.3), 30.0, 300.0);
        }
        // The same key again needs a release first.
        if prev.is_some_and(|p| p.key == s.key) {
            down = down.max(prev_up + clamp(log_normal(rng, 40.0, 0.4), 15.0, 200.0));
        }
        // Shift transitions.
        if s.shift && !shift_held {
            let lead = clamp(log_normal(rng, 65.0, 0.35), 20.0, 250.0);
            let floor = prev_down.map_or(0.0, |t| t + 10.0);
            let at = (down - lead).max(floor);
            down = down.max(at + 15.0);
            push(&mut events, "Shift", KeyEventType::Down, at);
            shift_held = true;
        } else if !s.shift && shift_held {
            let rel = shifted_up + clamp(log_normal(rng, 30.0, 0.5), 5.0, 200.0);
            down = down.max(rel + 10.0);
            push(&mut events, "Shift", KeyEventType::Up, rel);
            shift_held = false;
        }
        let hold = clamp(
            log_normal(
                rng,
                hold_median * key_hold_factor(&s.key, profile),
                profile.hold_sigma,
            ),
            30.0,
            400.0,
        );
        let up = down + hold;
        push(&mut events, &s.key, KeyEventType::Down, down);
        push(&mut events, &s.key, KeyEventType::Press, down);
        push(&mut events, &s.key, KeyEventType::Up, up);
        if s.shift {
            shifted_up = shifted_up.max(up);
        }
        prev_down = Some(down);
        prev_up = up;
        prev = Some(s);
    }
    if shift_held {
        let rel = shifted_up + clamp(log_normal(rng, 30.0, 0.5), 5.0, 200.0);
        push(&mut events, "Shift", KeyEventType::Up, rel);
    }
    // Time order; equal times keep insertion order (Down before Press).
    events.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.cmp(&b.1)));
    events
        .into_iter()
        .map(|(t, _, mut e)| {
            e.timestamp_ms = t.max(0.0).round() as u64;
            e
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::BehaviorProfile;
    use crate::rng::seeded_rng;

    fn profile() -> KeyboardProfile {
        BehaviorProfile::casual_user().keyboard
    }

    /// Replay Down events: Backspace deletes, Shift and Press do nothing.
    fn replay(events: &[KeystrokeEvent]) -> String {
        let mut s = String::new();
        for e in events.iter().filter(|e| e.event_type == KeyEventType::Down) {
            match e.key.as_str() {
                "Backspace" => {
                    s.pop();
                }
                "Shift" => {}
                "Enter" => s.push('\n'),
                "Tab" => s.push('\t'),
                k => s.push_str(k),
            }
        }
        s
    }

    #[test]
    fn events_are_monotonic() {
        let mut rng = seeded_rng(10);
        let events = generate_typing_events("Hello, World! It's 5 PM.", &profile(), &mut rng);
        for w in events.windows(2) {
            assert!(w[1].timestamp_ms >= w[0].timestamp_ms);
        }
    }

    #[test]
    fn replay_gives_the_text_for_many_seeds() {
        let text = "The Quick brown fox, said \"Hi!\"\nok\t(done)";
        let mut p = profile();
        p.error_rate = 0.15;
        let mut typos = 0;
        for seed in 0..200 {
            let mut rng = seeded_rng(seed);
            let ev = generate_typing_events(text, &p, &mut rng);
            assert_eq!(replay(&ev), text, "seed {seed}");
            if ev.iter().any(|e| e.key == "Backspace") {
                typos += 1;
            }
        }
        assert!(typos > 50, "{typos} runs with typos");
    }

    #[test]
    fn shift_wraps_shifted_characters() {
        for seed in 0..100 {
            let mut rng = seeded_rng(seed);
            let ev = generate_typing_events(
                "aB!c",
                &KeyboardProfile {
                    error_rate: 0.0,
                    ..profile()
                },
                &mut rng,
            );
            let mut shift = false;
            let mut down_keys = std::collections::HashSet::new();
            for e in &ev {
                match (e.key.as_str(), &e.event_type) {
                    ("Shift", KeyEventType::Down) => shift = true,
                    ("Shift", KeyEventType::Up) => shift = false,
                    (_, KeyEventType::Press) => {}
                    (k, KeyEventType::Down) => {
                        assert_eq!(
                            shift,
                            needs_shift(k.chars().next().unwrap()),
                            "seed {seed} {k}"
                        );
                        down_keys.insert(k.to_string());
                    }
                    (k, KeyEventType::Up) => {
                        if needs_shift(k.chars().next().unwrap()) {
                            assert!(shift, "seed {seed}: {k} released after Shift");
                        }
                        assert!(down_keys.remove(k));
                    }
                }
            }
            assert!(!shift);
        }
    }

    #[test]
    fn rollover_happens_and_same_key_never_overlaps() {
        let mut p = BehaviorProfile::power_user().keyboard;
        p.error_rate = 0.0;
        let mut overlaps = 0;
        for seed in 0..20 {
            let mut rng = seeded_rng(seed);
            let ev =
                generate_typing_events("the letters keep rolling over all the time", &p, &mut rng);
            let mut held: Vec<String> = Vec::new();
            for e in &ev {
                match e.event_type {
                    KeyEventType::Down => {
                        assert!(
                            !held.contains(&e.key),
                            "seed {seed}: {} pressed twice",
                            e.key
                        );
                        if !held.is_empty() {
                            overlaps += 1;
                        }
                        held.push(e.key.clone());
                    }
                    KeyEventType::Up => held.retain(|k| k != &e.key),
                    KeyEventType::Press => {}
                }
            }
        }
        assert!(overlaps > 20, "{overlaps} overlapping presses");
    }

    #[test]
    fn timing_statistics_are_human() {
        let mut p = profile();
        p.error_rate = 0.0;
        let mut rng = seeded_rng(77);
        let text = "a fairly long sentence that is typed by a casual typist at about sixty words per minute";
        let ev = generate_typing_events(text, &p, &mut rng);
        let downs: Vec<&KeystrokeEvent> = ev
            .iter()
            .filter(|e| e.event_type == KeyEventType::Down)
            .collect();
        let iki: Vec<f64> = downs
            .windows(2)
            .map(|w| (w[1].timestamp_ms - w[0].timestamp_ms) as f64)
            .collect();
        let mean = iki.iter().sum::<f64>() / iki.len() as f64;
        let sd = (iki.iter().map(|x| (x - mean).powi(2)).sum::<f64>() / iki.len() as f64).sqrt();
        // 60 WPM = 200 ms per character (word pauses add a little).
        assert!(mean > 140.0 && mean < 320.0, "mean IKI {mean}");
        assert!(sd / mean > 0.25, "cv {}", sd / mean);
        // Holds: median around 100 ms, spread out.
        let mut holds: Vec<f64> = Vec::new();
        for (i, d) in ev.iter().enumerate() {
            if d.event_type == KeyEventType::Down {
                let u = ev[i..]
                    .iter()
                    .find(|e| e.event_type == KeyEventType::Up && e.key == d.key)
                    .unwrap();
                holds.push((u.timestamp_ms - d.timestamp_ms) as f64);
            }
        }
        holds.sort_by(f64::total_cmp);
        let med = holds[holds.len() / 2];
        assert!(med > 70.0 && med < 140.0, "median hold {med}");
        assert!(holds[holds.len() - 1] - holds[0] > 30.0);
    }

    #[test]
    fn per_key_hold_factors_are_stable_per_session() {
        let mut p = profile();
        p.key_seed = 42;
        let a = key_hold_factor("j", &p);
        assert_eq!(a, key_hold_factor("J", &p));
        assert_ne!(a, key_hold_factor("k", &p));
        p.key_seed = 43;
        assert_ne!(a, key_hold_factor("j", &p));
        let spread: Vec<f64> = "abcdefghijklmnop"
            .chars()
            .map(|c| key_hold_factor(&c.to_string(), &p))
            .collect();
        assert!(spread.iter().all(|f| (0.5..2.0).contains(f)));
    }

    #[test]
    fn empty_text_returns_empty() {
        let mut rng = seeded_rng(13);
        assert!(generate_typing_events("", &profile(), &mut rng).is_empty());
    }

    #[test]
    fn bigram_multipliers() {
        assert!(bigram_multiplier('t', 'h') < 1.0);
        assert!(bigram_multiplier('z', 'x') > 1.0);
    }
}
