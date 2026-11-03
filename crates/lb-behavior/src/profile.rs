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
    /// Names accepted by [`BehaviorProfile::preset`].
    pub const PRESETS: [&'static str; 4] =
        ["casual_user", "power_user", "slow_reader", "mobile_like"];

    /// A preset by name (`persona.behavior.profile`).
    pub fn preset(name: &str) -> Option<Self> {
        match name {
            "casual_user" => Some(Self::casual_user()),
            "power_user" => Some(Self::power_user()),
            "slow_reader" => Some(Self::slow_reader()),
            "mobile_like" => Some(Self::mobile_like()),
            _ => None,
        }
    }

    /// The same profile with another scroll device (a persona's pointing
    /// device: a notched wheel on most desktops, a trackpad on laptops).
    pub fn with_scroll_type(mut self, t: ScrollType) -> Self {
        self.scroll.scroll_type = t;
        self
    }

    // ------------------------------------------------------------------
    // Preset constructors
    // ------------------------------------------------------------------

    /// A relaxed, average desktop user.
    ///
    /// Rationale: This is the most common profile on the web.  WPM ~55 is
    /// close to the 52 WPM average of Dhakal et al. 2018.  Fitts slope
    /// 210 ms/bit is a mouse throughput of about 4.8 bit/s (ISO 9241-9
    /// studies: 3.5-5).  A notched wheel; holds and IKIs at the dataset
    /// means.  Long dwell times represent reading before acting.
    pub fn casual_user() -> Self {
        Self {
            name: "casual_user".into(),
            mouse: MouseProfile {
                fitts_a_ms: 100.0,
                fitts_b_ms: 210.0,
                curvature: 0.08,
                path_noise: 0.012,
                overshoot_rate: 0.2,
                overshoot_distance_px: 18.0,
                device_hz: 125.0,
                display_hz: 60.0,
                press_hold_ms: 95.0,
                endpoint_spread: 1.0,
                micro_movement_enabled: true,
                micro_movement_std_px: 1.2,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 55.0,
                wpm_std: 8.0,
                error_rate: 0.03,
                correction_delay_ms: (250, 700),
                bigram_variation: true,
                iki_sigma: 0.4,
                hold_ms: 105.0,
                hold_sigma: 0.25,
                hesitation_rate: 0.06,
                hold_key_spread: 0.15,
                key_seed: 0,
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Wheel,
                notch_px: 100.0,
                notches_per_flick: 3.5,
                notch_interval_ms: 45.0,
                regrip_ms: 280.0,
                speed_mean_px_per_s: 1400.0,
                speed_std: 0.3,
                momentum_deceleration: 0.95,
                direction_change_probability: 0.1,
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
                fitts_a_ms: 60.0,
                fitts_b_ms: 160.0,
                curvature: 0.06,
                path_noise: 0.01,
                overshoot_rate: 0.15,
                overshoot_distance_px: 14.0,
                device_hz: 125.0,
                display_hz: 60.0,
                press_hold_ms: 80.0,
                endpoint_spread: 0.9,
                micro_movement_enabled: false,
                micro_movement_std_px: 0.5,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 95.0,
                wpm_std: 10.0,
                error_rate: 0.02,
                correction_delay_ms: (120, 350),
                bigram_variation: true,
                iki_sigma: 0.35,
                hold_ms: 90.0,
                hold_sigma: 0.22,
                hesitation_rate: 0.03,
                hold_key_spread: 0.12,
                key_seed: 0,
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Trackpad,
                notch_px: 100.0,
                notches_per_flick: 4.5,
                notch_interval_ms: 35.0,
                regrip_ms: 180.0,
                speed_mean_px_per_s: 2200.0,
                speed_std: 0.3,
                momentum_deceleration: 0.955,
                direction_change_probability: 0.05,
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
                fitts_a_ms: 200.0,
                fitts_b_ms: 280.0,
                curvature: 0.1,
                path_noise: 0.015,
                overshoot_rate: 0.3,
                overshoot_distance_px: 25.0,
                device_hz: 125.0,
                display_hz: 60.0,
                press_hold_ms: 120.0,
                endpoint_spread: 1.2,
                micro_movement_enabled: true,
                micro_movement_std_px: 2.5,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 32.0,
                wpm_std: 6.0,
                error_rate: 0.06,
                correction_delay_ms: (500, 1500),
                bigram_variation: false,
                iki_sigma: 0.5,
                hold_ms: 130.0,
                hold_sigma: 0.3,
                hesitation_rate: 0.12,
                hold_key_spread: 0.18,
                key_seed: 0,
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Wheel,
                notch_px: 100.0,
                notches_per_flick: 2.5,
                notch_interval_ms: 60.0,
                regrip_ms: 400.0,
                speed_mean_px_per_s: 900.0,
                speed_std: 0.3,
                momentum_deceleration: 0.94,
                direction_change_probability: 0.15,
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
                fitts_a_ms: 150.0,
                fitts_b_ms: 220.0,
                curvature: 0.05,
                path_noise: 0.01,
                overshoot_rate: 0.1,
                overshoot_distance_px: 8.0,
                device_hz: 125.0,
                display_hz: 60.0,
                press_hold_ms: 90.0,
                endpoint_spread: 1.1,
                micro_movement_enabled: false,
                micro_movement_std_px: 0.0,
            },
            keyboard: KeyboardProfile {
                wpm_mean: 40.0,
                wpm_std: 10.0,
                error_rate: 0.02,
                correction_delay_ms: (50, 150),
                bigram_variation: false,
                iki_sigma: 0.45,
                hold_ms: 95.0,
                hold_sigma: 0.25,
                hesitation_rate: 0.08,
                hold_key_spread: 0.15,
                key_seed: 0,
            },
            scroll: ScrollProfile {
                scroll_type: ScrollType::Trackpad,
                notch_px: 100.0,
                notches_per_flick: 3.0,
                notch_interval_ms: 45.0,
                regrip_ms: 250.0,
                speed_mean_px_per_s: 1800.0,
                speed_std: 0.35,
                momentum_deceleration: 0.95,
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

    #[test]
    fn presets_by_name() {
        for name in BehaviorProfile::PRESETS {
            assert_eq!(BehaviorProfile::preset(name).unwrap().name, name);
        }
        assert!(BehaviorProfile::preset("nobody").is_none());
        let p = BehaviorProfile::casual_user().with_scroll_type(ScrollType::Trackpad);
        assert_eq!(p.scroll.scroll_type, ScrollType::Trackpad);
    }

    #[test]
    fn partial_json_fills_defaults() {
        let p: MouseProfile = serde_json::from_str(r#"{"fitts_b_ms": 300.0}"#).unwrap();
        assert_eq!(p.fitts_b_ms, 300.0);
        assert_eq!(p.display_hz, 60.0);
    }
}
