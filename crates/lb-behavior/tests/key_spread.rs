//! How often the detect-bench keystroke spread rules (`beh_key_rhythm`,
//! `beh_key_dwell` in `tests/detect-bench/sites/collector.js`) fire on the
//! keyboard model's own output, by chance, for the text typed on one page.
//! The rules judge a robust spread (median absolute deviation over the
//! median) of the page's key-down intervals and key holds; with few keys
//! that estimate is noisy.

use lb_behavior::BehaviorProfile;
use lb_behavior::keyboard::{KeyEventType, KeystrokeEvent, generate_typing_events};
use lb_behavior::rng::{mix_seed, seeded_rng};

fn median(a: &[f64]) -> f64 {
    let mut s = a.to_vec();
    s.sort_by(f64::total_cmp);
    let m = s.len() / 2;
    if s.len() % 2 == 1 {
        s[m]
    } else {
        (s[m - 1] + s[m]) / 2.0
    }
}

/// collector.js `rcv`: MAD / median.
fn rcv(a: &[f64]) -> Option<f64> {
    if a.len() < 2 {
        return None;
    }
    let m = median(a);
    if m == 0.0 {
        return None;
    }
    let d: Vec<f64> = a.iter().map(|x| (x - m).abs()).collect();
    Some(median(&d) / m)
}

/// One field's keys as the collector records them: absolute time, key
/// name (letters in a password field become "x"), down or up.
fn field(events: &[KeystrokeEvent], t0: f64, password: bool) -> Vec<(f64, String, bool)> {
    events
        .iter()
        .filter(|e| e.event_type != KeyEventType::Press)
        .map(|e| {
            let k = if password && e.key.chars().count() == 1 {
                "x".into()
            } else {
                e.key.clone()
            };
            (
                t0 + e.timestamp_ms as f64,
                k,
                e.event_type == KeyEventType::Down,
            )
        })
        .collect()
}

/// (rhythm fired, dwell fired) for the keys of one page.
fn rules(keys: &[(f64, String, bool)]) -> (bool, bool) {
    let downs: Vec<&(f64, String, bool)> = keys.iter().filter(|k| k.2).collect();
    let ups: Vec<&(f64, String, bool)> = keys.iter().filter(|k| !k.2).collect();
    let iki: Vec<f64> = downs
        .windows(2)
        .map(|w| w[1].0 - w[0].0)
        .filter(|g| *g < 2000.0)
        .collect();
    let rhythm = iki.len() >= 5 && rcv(&iki).is_some_and(|s| s < 0.1);
    let mut dwell = Vec::new();
    for d in &downs {
        if let Some(u) = ups.iter().find(|u| u.1 == d.1 && u.0 >= d.0) {
            dwell.push(u.0 - d.0);
        }
    }
    let dwell_fired = (dwell.len() >= 3 && median(&dwell) < 10.0)
        || (dwell.len() >= 5 && rcv(&dwell).is_some_and(|s| s < 0.08));
    (rhythm, dwell_fired)
}

/// Chance rates over `n` sessions for a page where `fields` are typed one
/// after another (each a separate humanType call, as lb-page does).
fn rates(fields: &[(&str, bool)], n: u64) -> (f64, f64, f64) {
    let base = BehaviorProfile::casual_user();
    let (mut r, mut d, mut any) = (0u32, 0u32, 0u32);
    for s in 0..n {
        let session = mix_seed(0x5eed, s);
        let mut p = base.keyboard.clone();
        p.key_seed = session;
        let mut keys = Vec::new();
        let mut t0 = 0.0;
        for (call, (text, pw)) in fields.iter().enumerate() {
            let mut rng = seeded_rng(mix_seed(session, call as u64));
            let ev = generate_typing_events(text, &p, &mut rng);
            let f = field(&ev, t0, *pw);
            t0 = f.iter().map(|k| k.0).fold(t0, f64::max) + 2500.0;
            keys.extend(f);
        }
        let (a, b) = rules(&keys);
        r += a as u32;
        d += b as u32;
        any += (a || b) as u32;
    }
    let n = n as f64;
    (r as f64 / n, d as f64 / n, any as f64 / n)
}

/// Pooled statistics of the model over many sessions, to compare with
/// published typing data (printed with `--nocapture`).
fn pooled(text: &str, n: u64) -> (f64, f64, f64, f64) {
    let base = BehaviorProfile::casual_user();
    let (mut holds, mut ikis) = (Vec::new(), Vec::new());
    for s in 0..n {
        let session = mix_seed(0xda7a, s);
        let mut p = base.keyboard.clone();
        p.key_seed = session;
        p.error_rate = 0.0;
        let ev = generate_typing_events(text, &p, &mut seeded_rng(session));
        let keys = field(&ev, 0.0, false);
        let downs: Vec<_> = keys.iter().filter(|k| k.2).collect();
        ikis.extend(downs.windows(2).map(|w| w[1].0 - w[0].0));
        for d in &downs {
            if let Some(u) = keys.iter().find(|u| !u.2 && u.1 == d.1 && u.0 >= d.0) {
                holds.push(u.0 - d.0);
            }
        }
    }
    let mean = |a: &[f64]| a.iter().sum::<f64>() / a.len() as f64;
    let cv = |a: &[f64]| {
        let m = mean(a);
        (a.iter().map(|x| (x - m).powi(2)).sum::<f64>() / a.len() as f64).sqrt() / m
    };
    (mean(&holds), cv(&holds), mean(&ikis), cv(&ikis))
}

/// The model's holds and intervals are as spread out as human typing
/// (Dhakal et al. 2018: mean hold 116 ms, mean IKI 239 ms; IKI and hold
/// distributions are wide and right-skewed), so the spread rules almost
/// never fire on a realistic amount of text. On a single six-letter word
/// the MAD/median estimate is noisy and they fire by chance; that is a
/// property of the rule on short samples, not of the model.
#[test]
fn spread_rules_fire_only_by_chance_on_short_text() {
    let (hold_mean, hold_cv, iki_mean, iki_cv) =
        pooled("the quick brown fox jumps over the lazy dog", 400);
    println!(
        "pooled: hold mean {hold_mean:.0} ms cv {hold_cv:.2}; iki mean {iki_mean:.0} ms cv {iki_cv:.2}"
    );
    assert!((80.0..150.0).contains(&hold_mean), "hold mean {hold_mean}");
    assert!(hold_cv > 0.2, "hold cv {hold_cv}");
    assert!((150.0..350.0).contains(&iki_mean), "iki mean {iki_mean}");
    assert!(iki_cv > 0.35, "iki cv {iki_cv}");

    let pages: [&[(&str, bool)]; 4] = [
        &[("jacket", false)],
        &[("waterproof rain jacket", false)],
        &[("mens waterproof rain jacket", false)],
        // detect-bench T-010 login page: email, then password.
        &[
            ("jordan.miller84@example.com", false),
            ("Tr4il-runner!", true),
        ],
    ];
    let mut out = Vec::new();
    for page in pages {
        let (r, d, any) = rates(page, 4000);
        println!(
            "{page:?}: rhythm {:.1}% dwell {:.1}% either {:.1}%",
            r * 100.0,
            d * 100.0,
            any * 100.0
        );
        out.push(any);
    }
    // Six keys: a sizeable chance (about a quarter).
    assert!(out[0] > 0.1, "jacket {}", out[0]);
    // Three words or two login fields: rare.
    for any in &out[1..] {
        assert!(*any < 0.02, "{out:?}");
    }
}
