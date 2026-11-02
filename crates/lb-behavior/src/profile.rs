// profile.rs — BehaviorProfile presets (vendored from stealth-browser)
//
// A BehaviorProfile bundles all sub-profiles into a single serialisable
// struct.  The replay layer (lb-cdp) picks (or constructs) a profile per
// session so that different "users" produced by the engine have consistent
// behavioural fingerprints across the entire session rather than random
// characteristics per action.
//
// Consistency is critical: bot detectors build per-session feature vectors
// (e.g. average WPM, mouse speed distribution, scroll behaviour) and flag
// sessions whose features are internally inconsistent — e.g. 120 WPM typing
// with hunt-and-peck mouse movements.

use serde::{Deserialize, Serialize};

use crate::keyboard::KeyboardProfile;
use crate::mouse::MouseProfile;
use crate::scroll::{ScrollProfile, ScrollType};
use crate::timing::TimingProfile;

/// The top-level session personality configuration.
///
/// Every generated event in a session is derived from the same profile,
/// ensuring cross-modality consistency that is hard for ML-based detectors
/// to distinguish from a real human.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BehaviorProfile {
    /// Human-readable label (not used in generation logic).
    pub name: String,
    pub mouse: MouseProfile,
    pub keyboard: KeyboardProfile,
    pub scroll: ScrollProfile,
    pub timing: TimingProfile,
}

impl BehaviorProfile {
    // ------------------------------------------------------------------
    // Preset constructors
    // ------------------------------------------------------------------

    /// A relaxed, average desktop user.
    ///
    /// Rationale: This is the most common profile on the web.  WPM ~60
    /// matches the widely-cited average adult typing speed.  Mouse speed is
    /// moderate with occasional overshoots (humans frequently overshoot and
    /// correct, especially when targeting small elements).  Long dwell times
    /// represent reading and considering content before acting.
    pub fn casual_user() -> Self {
        Self {
            name: "casual_user".into(),
            mouse: MouseProfile {
                speed_mean_px_per_s: 800.0,
                speed_std: 200.0,
                overshoot_rate: 0.25,
                overshoot_distance_px: 18.0,
                micro_movement_enabled: true,
                micro_movement_std_px: 1.2,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 60.0,
                wpm_std: 10.0,
                error_rate: 0.04,
                correction_delay_ms: (200, 600),
                bigram_variation: true,
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Wheel,
                speed_mean_px_per_s: 1200.0,
                speed_std: 300.0,
                momentum_deceleration: 0.85,
                direction_change_probability: 0.05,
            },
            timing: TimingProfile {
                first_interaction_ms: (800, 3000),
                dwell_ms: (4000, 20000),
                reading_speed_wpm: 250.0,
                action_think_ms: (300, 1200),
            },
        }
    }

    /// A fast, experienced desktop user — developer, power-user, etc.
    ///
    /// Rationale: WPM ~100 is in the top ~15 % of typists.  High mouse speed
    /// with low overshoot reflects practiced motor control.  Short dwell and
    /// think times indicate familiarity with the interface.  Bigram variation
    /// is pronounced because fast typists develop strong finger-pair habits.
    pub fn power_user() -> Self {
        Self {
            name: "power_user".into(),
            mouse: MouseProfile {
                speed_mean_px_per_s: 1600.0,
                speed_std: 300.0,
                overshoot_rate: 0.10,
                overshoot_distance_px: 8.0,
                micro_movement_enabled: false, // stays still while reading
                micro_movement_std_px: 0.5,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 100.0,
                wpm_std: 12.0,
                error_rate: 0.02,
                correction_delay_ms: (80, 250),
                bigram_variation: true,
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Trackpad,
                speed_mean_px_per_s: 2000.0,
                speed_std: 400.0,
                momentum_deceleration: 0.78,
                direction_change_probability: 0.02,
            },
            timing: TimingProfile {
                first_interaction_ms: (300, 1000),
                dwell_ms: (1500, 8000),
                reading_speed_wpm: 400.0,
                action_think_ms: (100, 500),
            },
        }
    }

    /// A slow, cautious reader — elderly user, non-native speaker, etc.
    ///
    /// Rationale: WPM ~35 corresponds to hunt-and-peck typists or slow
    /// deliberate typers.  High overshoot rate with long corrections reflects
    /// less precise motor control.  Long dwell times model careful reading.
    /// High error rate and long correction delays are realistic for this cohort.
    pub fn slow_reader() -> Self {
        Self {
            name: "slow_reader".into(),
            mouse: MouseProfile {
                speed_mean_px_per_s: 400.0,
                speed_std: 150.0,
                overshoot_rate: 0.40,
                overshoot_distance_px: 30.0,
                micro_movement_enabled: true,
                micro_movement_std_px: 2.5, // more drift while hovering/reading
            },
            keyboard: KeyboardProfile {
                wpm_mean: 35.0,
                wpm_std: 8.0,
                error_rate: 0.08,
                correction_delay_ms: (500, 1500),
                bigram_variation: false, // more uniform; no well-practiced bigrams
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Wheel,
                speed_mean_px_per_s: 600.0,
                speed_std: 200.0,
                momentum_deceleration: 0.90,
                direction_change_probability: 0.12, // re-reads sections
            },
            timing: TimingProfile {
                first_interaction_ms: (1500, 6000),
                dwell_ms: (15000, 60000),
                reading_speed_wpm: 150.0,
                action_think_ms: (800, 3000),
            },
        }
    }

    /// A mobile / touch-first user (smartphone or tablet).
    ///
    /// Rationale: Touch interfaces have no physical mouse.  "Mouse" events are
    /// replaced by tap/swipe gestures modelled as very short, near-vertical
    /// paths.  Typing is slower because soft keyboards are less efficient than
    /// physical ones; auto-correct produces characteristic burst-then-pause
    /// patterns which we approximate with a low error_rate (auto-correct hides
    /// real errors) and moderate WPM.  Scrolling is always trackpad-style
    /// because touch screens produce momentum scroll events.
    pub fn mobile_like() -> Self {
        Self {
            name: "mobile_like".into(),
            mouse: MouseProfile {
                // Short, slow "swipe" movements rather than cross-screen arcs.
                speed_mean_px_per_s: 350.0,
                speed_std: 120.0,
                overshoot_rate: 0.15,
                overshoot_distance_px: 10.0,
                micro_movement_enabled: false, // no hover jitter on touch
                micro_movement_std_px: 0.0,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 45.0,
                wpm_std: 12.0,
                // Auto-correct intercepts many typos; apparent error rate is low
                // but correction delays are short (auto-correct is instant).
                error_rate: 0.02,
                correction_delay_ms: (50, 150),
                bigram_variation: false,
            },
            scroll: ScrollProfile {
                // Touch always produces smooth momentum scroll.
                scroll_type: ScrollType::Trackpad,
                speed_mean_px_per_s: 1800.0,
                speed_std: 600.0,
                momentum_deceleration: 0.82,
                direction_change_probability: 0.08,
            },
            timing: TimingProfile {
                first_interaction_ms: (500, 2000),
                dwell_ms: (3000, 15000),
                reading_speed_wpm: 200.0,
                action_think_ms: (200, 800),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn all_presets_serialize() {
        for profile in [
            BehaviorProfile::casual_user(),
            BehaviorProfile::power_user(),
            BehaviorProfile::slow_reader(),
            BehaviorProfile::mobile_like(),
        ] {
            let json = serde_json::to_string(&profile).expect("serialize");
            let back: BehaviorProfile = serde_json::from_str(&json).expect("deserialize");
            assert_eq!(back.name, profile.name);
        }
    }
}
