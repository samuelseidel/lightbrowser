//! Human-like input simulation: Bezier mouse paths, bigram-aware typing and
//! momentum scrolling, all seeded and reproducible.
//!
//! Vendored from the `behavior` crate in stealth-browser (GeckoPhantom) at
//! commit `2cb3ba0`. Changes from upstream:
//!
//! - Renamed from `geckophantom-behavior`; edition 2024; `rlib` only (no
//!   `cdylib`).
//! - `rand` 0.8 to 0.9 (`gen` became `random`). `SmallRng` is a different
//!   generator in 0.9, so a given seed yields different events than upstream.
//!   Same seed still gives the same events within this crate.
//! - Doc comments no longer refer to ShadowChannel; the consumer is lb-cdp.
//! - rustfmt and clippy `-D warnings` clean.
//! - New [`cdp`] module: turns an [`InputEvent`] stream into CDP
//!   `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent` /
//!   `Input.insertText` parameters with absolute time offsets, so lb-page
//!   can replay a stream on the real clock (`LightBrowser.human*`).
//! - M5: the generators were rewritten from first principles; each module's
//!   header names the model and its sources. [`mouse`]: Fitts' law
//!   durations, primary + corrective submovements (Meyer et al. 1988),
//!   minimum-jerk speed (Flash & Hogan 1985), curved paths, endpoint
//!   scatter from the target size (ISO 9241-9 effective width), 125 Hz
//!   device reports coalesced to 60 Hz frames, whole pixels, button holds.
//!   [`keyboard`]: log-normal down-to-down intervals and holds (Dhakal et
//!   al. 2018), rollover, Shift around shifted characters, typos noticed a
//!   few keys later. [`scroll`]: notched wheels in flicks, trackpad swipes
//!   with momentum. Think times are log-normal. Profiles carry the new
//!   parameters ([`BehaviorProfile::preset`] by persona name).
//!
//! ## Architecture
//!
//! Low-level generators (one per input modality: [`mouse`], [`keyboard`],
//! [`scroll`]) are coordinated by the high-level API in this file, which takes
//! a [`BehaviorProfile`] (the "personality" of the simulated user) and a seed
//! that makes the call reproducible.
//!
//! ## Event model
//!
//! All events are unified into [`InputEvent`]. Each generator stamps its own
//! events relative to the start of its sequence; [`InputEvent::Wait`] carries
//! the explicit pauses between sequences. [`cdp::events_to_cdp`] folds both
//! into one absolute timeline.
//!
//! ## Seeding strategy
//!
//! Each high-level call takes an independent `seed: u64`. Callers should
//! derive per-call seeds from one session seed with a deterministic mixing
//! function (e.g. `session_seed ^ (action_index * 0x9e3779b97f4a7c15)`) so two
//! actions never share an event sequence while the whole session stays
//! reproducible from one root seed.

pub mod cdp;
pub mod keyboard;
pub mod mouse;
pub mod profile;
pub mod rng;
pub mod scroll;
pub mod timing;

// ---------------------------------------------------------------------------
// Re-exports — public API surface
// ---------------------------------------------------------------------------

pub use keyboard::{
    KeyEventType, KeyboardProfile, KeystrokeEvent, generate_typing_events, needs_shift,
};
pub use mouse::{
    Button, ClickOptions, MouseEvent, MouseEventType, MouseProfile, Point, TargetBox,
    generate_click, generate_click_in, generate_micro_movements, generate_mouse_path,
};
pub use profile::BehaviorProfile;
pub use scroll::{ScrollDirection, ScrollEvent, ScrollProfile, ScrollType, generate_scroll};
pub use timing::{
    TimingProfile, reading_time_ms, sample_dwell, sample_first_interaction,
    sample_navigation_timing, sample_think_time,
};

// ---------------------------------------------------------------------------
// Unified event enum
// ---------------------------------------------------------------------------

/// Wrapper for a `Wait` event so that internal tagging works correctly with
/// serde.  Serde's `tag` format requires every variant to serialise as a map;
/// bare primitives (u64) are not maps, so we wrap the duration in a struct.
///
/// `Wait` events are explicit pauses that the consumer must honour — they
/// represent the cognitive and perceptual delays between actions that are
/// critical for passing timing-based detectors.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct WaitEvent {
    /// Duration to pause in milliseconds before processing the next event.
    pub duration_ms: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
/// The single unified event type produced by all high-level API functions.
///
/// The replay layer (lb-cdp) consumes this enum, usually via
/// [`cdp::events_to_cdp`], and dispatches each variant as CDP input.
pub enum InputEvent {
    /// A mouse move, button, or click event.
    Mouse(MouseEvent),
    /// A keyboard down, press, or up event.
    Keyboard(KeystrokeEvent),
    /// A scroll wheel or trackpad event.
    Scroll(ScrollEvent),
    /// An explicit pause: the consumer should sleep for `duration_ms`
    /// milliseconds before processing the next event.
    Wait(WaitEvent),
}

// ---------------------------------------------------------------------------
// High-level API
// ---------------------------------------------------------------------------

/// Pointer movement from `from` to `to` with no think time (hovering,
/// repositioning). Ends exactly at `to` (whole pixels).
pub fn human_move_events(
    from: Point,
    to: Point,
    profile: &BehaviorProfile,
    seed: u64,
) -> Vec<InputEvent> {
    let mut rng = rng::seeded_rng(seed);
    mouse::generate_mouse_path(from, to, &profile.mouse, &mut rng)
        .into_iter()
        .map(InputEvent::Mouse)
        .collect()
}

/// A think-time pause, then a movement into `target` and a click at a
/// point inside it chosen by the profile (not its exact centre). Returns
/// the events and the clicked point.
pub fn human_click_box_events(
    from: Point,
    target: &TargetBox,
    opts: ClickOptions,
    profile: &BehaviorProfile,
    seed: u64,
) -> (Vec<InputEvent>, Point) {
    let mut rng = rng::seeded_rng(seed);
    let think_ms = timing::sample_think_time(&profile.timing, &mut rng);
    let mut events = vec![InputEvent::Wait(WaitEvent {
        duration_ms: think_ms,
    })];
    let (mouse_events, at) = mouse::generate_click_in(from, target, opts, &profile.mouse, &mut rng);
    events.extend(mouse_events.into_iter().map(InputEvent::Mouse));
    (events, at)
}

/// Generate all events required to perform a human-like click on a target.
///
/// The sequence is:
///   1. A `Wait` event representing the think-time before initiating movement.
///   2. `Mouse(Move)` events along a curved, Fitts-timed path from `from`
///      to a point near `target`.
///   3. `Mouse(Down)`, `Mouse(Up)`, `Mouse(Click)` events at that point.
///
/// # Arguments
/// * `from`    — current cursor position
/// * `target`  — centre of the element to click (treated as a 24 px box;
///   use [`human_click_box_events`] when the element's box is known)
/// * `profile` — session behaviour profile
/// * `seed`    — RNG seed for this specific action
pub fn human_click_events(
    from: Point,
    target: Point,
    profile: &BehaviorProfile,
    seed: u64,
) -> Vec<InputEvent> {
    let mut rng = rng::seeded_rng(seed);
    let mut events: Vec<InputEvent> = Vec::new();

    // Think-time pause before the user initiates movement.
    // This models the moment of deciding to click and locating the target.
    let think_ms = timing::sample_think_time(&profile.timing, &mut rng);
    events.push(InputEvent::Wait(WaitEvent {
        duration_ms: think_ms,
    }));

    // Generate mouse path + click events (mouse timestamps are relative to
    // the start of the mouse movement, not the think-time).
    let mouse_events = mouse::generate_click(from, target, &profile.mouse, &mut rng);
    events.extend(mouse_events.into_iter().map(InputEvent::Mouse));

    events
}

/// Generate all events required to type `text` into a focused input field.
///
/// The sequence is:
///   1. A `Wait` event for think-time (the user reads the field label, etc.).
///   2. A time-ordered sequence of `Keyboard` events (Down → Press → Up per
///      key, Shift around shifted characters, keys may overlap), with
///      log-normal intervals, bigram effects and occasional typos fixed
///      with Backspace.
///
/// # Arguments
/// * `text`    — the text to type (may contain \n for Enter, \t for Tab)
/// * `profile` — session behaviour profile
/// * `seed`    — RNG seed for this specific action
pub fn human_type_events(text: &str, profile: &BehaviorProfile, seed: u64) -> Vec<InputEvent> {
    let mut rng = rng::seeded_rng(seed);
    let mut events: Vec<InputEvent> = Vec::new();

    // Think-time: the user reads the field, decides what to type, positions
    // their hands.
    let think_ms = timing::sample_think_time(&profile.timing, &mut rng);
    events.push(InputEvent::Wait(WaitEvent {
        duration_ms: think_ms,
    }));

    let key_events = keyboard::generate_typing_events(text, &profile.keyboard, &mut rng);
    events.extend(key_events.into_iter().map(InputEvent::Keyboard));

    events
}

/// Generate scroll events to scroll `amount_px` in `direction`.
///
/// The sequence is:
///   1. A `Wait` event for think-time (the user decides to scroll).
///   2. `Scroll` events from the notched-wheel model or the trackpad model,
///      as set by `profile.scroll.scroll_type`.
///
/// # Arguments
/// * `direction` — scroll direction
/// * `amount_px` — total pixels to scroll (absolute value)
/// * `profile`   — session behaviour profile
/// * `seed`      — RNG seed for this specific action
pub fn human_scroll_events(
    direction: ScrollDirection,
    amount_px: f64,
    profile: &BehaviorProfile,
    seed: u64,
) -> Vec<InputEvent> {
    let mut rng = rng::seeded_rng(seed);
    let mut events: Vec<InputEvent> = Vec::new();

    // Think-time: brief pause before the user starts scrolling.
    let think_ms = timing::sample_think_time(&profile.timing, &mut rng);
    events.push(InputEvent::Wait(WaitEvent {
        duration_ms: think_ms,
    }));

    let scroll_events = scroll::generate_scroll(direction, amount_px, &profile.scroll, &mut rng);
    events.extend(scroll_events.into_iter().map(InputEvent::Scroll));

    events
}

/// Generate a complete "page visit" event sequence.
///
/// This higher-order function composes the individual generators into a
/// realistic session segment:
///   1. Wait for first-interaction delay (log-normal, simulates reading the page).
///   2. Move mouse to somewhere on the page (random point in the viewport).
///   3. Scroll down to explore content.
///   4. Move to `target` and click.
///   5. If `text` is provided, type it.
///
/// # Arguments
/// * `viewport`        — (width, height) of the browser viewport in pixels
/// * `target`          — click target position
/// * `text`            — optional text to type after clicking
/// * `content_length`  — approximate character count of visible page text
///   (used to derive a plausible first-interaction delay)
/// * `profile`         — session behaviour profile
/// * `seed`            — root RNG seed for this page visit
pub fn human_page_visit(
    viewport: (f64, f64),
    target: Point,
    text: Option<&str>,
    content_length: usize,
    profile: &BehaviorProfile,
    seed: u64,
) -> Vec<InputEvent> {
    // Derive sub-seeds deterministically from the root seed so that each
    // action is independent but the session is reproducible from `seed`.
    // We use a simple Weyl sequence multiplier.
    const WEYL: u64 = 0x9e3779b97f4a7c15;
    let seed_move = seed.wrapping_add(WEYL);
    let seed_scroll = seed.wrapping_add(WEYL.wrapping_mul(2));
    let seed_click = seed.wrapping_add(WEYL.wrapping_mul(3));
    let seed_type = seed.wrapping_add(WEYL.wrapping_mul(4));

    let mut rng = rng::seeded_rng(seed);
    let mut events: Vec<InputEvent> = Vec::new();

    // 1. First-interaction delay — reading the page before doing anything.
    //    We take the maximum of the sampled value and the reading time to
    //    ensure the dwell is at least plausible for the content.
    let sampled_fi = timing::sample_first_interaction(&profile.timing, &mut rng);
    let reading_fi = timing::reading_time_ms(content_length / 3, &profile.timing);
    let first_interaction = sampled_fi.max(reading_fi);
    events.push(InputEvent::Wait(WaitEvent {
        duration_ms: first_interaction,
    }));

    // 2. Move the mouse from an initial rest position (top-left area) to a
    //    random mid-page position (simulating the user moving the mouse
    //    while reading).
    let initial_pos = Point::new(
        rng::uniform(&mut rng, 50.0, viewport.0 * 0.3),
        rng::uniform(&mut rng, 50.0, viewport.1 * 0.3),
    );
    let mid_pos = Point::new(
        rng::uniform(&mut rng, viewport.0 * 0.2, viewport.0 * 0.8),
        rng::uniform(&mut rng, viewport.1 * 0.2, viewport.1 * 0.6),
    );
    let move_events = mouse::generate_mouse_path(
        initial_pos,
        mid_pos,
        &profile.mouse,
        &mut rng::seeded_rng(seed_move),
    );
    events.extend(move_events.into_iter().map(InputEvent::Mouse));

    // 3. Scroll down to explore page content (roughly half a viewport).
    let scroll_amount = rng::uniform(&mut rng, viewport.1 * 0.3, viewport.1 * 0.8);
    events.extend(human_scroll_events(
        ScrollDirection::Down,
        scroll_amount,
        profile,
        seed_scroll,
    ));

    // 4. Click the target.
    events.extend(human_click_events(mid_pos, target, profile, seed_click));

    // 5. Optionally type text.
    if let Some(t) = text
        && !t.is_empty()
    {
        events.extend(human_type_events(t, profile, seed_type));
    }

    events
}

// ---------------------------------------------------------------------------
// JSON serialisation helpers (for replay IPC)
// ---------------------------------------------------------------------------

/// Serialise a `Vec<InputEvent>` to a compact JSON string.
///
/// The replay layer (lb-cdp) uses this to pass event streams over its
/// internal IPC channel to the browser automation worker.
pub fn events_to_json(events: &[InputEvent]) -> Result<String, serde_json::Error> {
    serde_json::to_string(events)
}

/// Deserialise a JSON string back into a `Vec<InputEvent>`.
pub fn events_from_json(json: &str) -> Result<Vec<InputEvent>, serde_json::Error> {
    serde_json::from_str(json)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_click_events_not_empty() {
        let profile = BehaviorProfile::casual_user();
        let events =
            human_click_events(Point::new(0.0, 0.0), Point::new(400.0, 300.0), &profile, 42);
        assert!(!events.is_empty());
        // First event must be a Wait (think-time).
        assert!(matches!(events[0], InputEvent::Wait(_)));
        // Must contain at least one click.
        let has_click = events
            .iter()
            .any(|e| matches!(e, InputEvent::Mouse(m) if m.event_type == MouseEventType::Click));
        assert!(has_click);
    }

    #[test]
    fn human_type_events_not_empty() {
        let profile = BehaviorProfile::power_user();
        let events = human_type_events("hello", &profile, 99);
        assert!(!events.is_empty());
        assert!(matches!(events[0], InputEvent::Wait(_)));
        let has_key = events.iter().any(|e| matches!(e, InputEvent::Keyboard(_)));
        assert!(has_key);
    }

    #[test]
    fn human_scroll_events_not_empty() {
        let profile = BehaviorProfile::casual_user();
        let events = human_scroll_events(ScrollDirection::Down, 500.0, &profile, 77);
        assert!(!events.is_empty());
        assert!(matches!(events[0], InputEvent::Wait(_)));
        let has_scroll = events.iter().any(|e| matches!(e, InputEvent::Scroll(_)));
        assert!(has_scroll);
    }

    #[test]
    fn json_round_trip() {
        let profile = BehaviorProfile::casual_user();
        let events = human_click_events(
            Point::new(100.0, 100.0),
            Point::new(500.0, 400.0),
            &profile,
            1234,
        );
        let json = events_to_json(&events).expect("serialize");
        let back = events_from_json(&json).expect("deserialize");
        assert_eq!(events.len(), back.len());
    }

    #[test]
    fn determinism_across_calls() {
        let profile = BehaviorProfile::casual_user();
        let a = human_click_events(Point::new(0.0, 0.0), Point::new(300.0, 200.0), &profile, 55);
        let b = human_click_events(Point::new(0.0, 0.0), Point::new(300.0, 200.0), &profile, 55);
        let a_json = events_to_json(&a).unwrap();
        let b_json = events_to_json(&b).unwrap();
        assert_eq!(a_json, b_json, "same seed must produce identical output");
    }

    #[test]
    fn different_seeds_produce_different_output() {
        let profile = BehaviorProfile::casual_user();
        let a = human_click_events(Point::new(0.0, 0.0), Point::new(300.0, 200.0), &profile, 1);
        let b = human_click_events(Point::new(0.0, 0.0), Point::new(300.0, 200.0), &profile, 2);
        let a_json = events_to_json(&a).unwrap();
        let b_json = events_to_json(&b).unwrap();
        assert_ne!(
            a_json, b_json,
            "different seeds should produce different output"
        );
    }

    #[test]
    fn all_profiles_produce_events() {
        let profiles = [
            BehaviorProfile::casual_user(),
            BehaviorProfile::power_user(),
            BehaviorProfile::slow_reader(),
            BehaviorProfile::mobile_like(),
        ];
        for profile in &profiles {
            let click =
                human_click_events(Point::new(50.0, 50.0), Point::new(640.0, 360.0), profile, 1);
            assert!(
                !click.is_empty(),
                "profile {} produced no click events",
                profile.name
            );

            let typing = human_type_events("test", profile, 2);
            assert!(
                !typing.is_empty(),
                "profile {} produced no typing events",
                profile.name
            );

            let scroll = human_scroll_events(ScrollDirection::Down, 300.0, profile, 3);
            assert!(
                !scroll.is_empty(),
                "profile {} produced no scroll events",
                profile.name
            );
        }
    }

    #[test]
    fn click_box_lands_inside() {
        let profile = BehaviorProfile::casual_user();
        let target = TargetBox::new(200.0, 300.0, 80.0, 30.0);
        let (events, at) = human_click_box_events(
            Point::new(5.0, 5.0),
            &target,
            ClickOptions::default(),
            &profile,
            9,
        );
        assert!(target.contains(at));
        assert!(matches!(events[0], InputEvent::Wait(_)));
        let (again, at2) = human_click_box_events(
            Point::new(5.0, 5.0),
            &target,
            ClickOptions::default(),
            &profile,
            9,
        );
        assert_eq!(at, at2);
        assert_eq!(
            events_to_json(&events).unwrap(),
            events_to_json(&again).unwrap()
        );
    }

    #[test]
    fn move_events_end_at_target() {
        let profile = BehaviorProfile::power_user();
        let ev = human_move_events(Point::new(0.0, 0.0), Point::new(321.0, 123.0), &profile, 4);
        let Some(InputEvent::Mouse(last)) = ev.last() else {
            panic!("no moves")
        };
        assert_eq!((last.x, last.y), (321.0, 123.0));
    }

    #[test]
    fn page_visit_has_all_event_types() {
        let profile = BehaviorProfile::casual_user();
        let events = human_page_visit(
            (1280.0, 720.0),
            Point::new(640.0, 400.0),
            Some("search term"),
            2000,
            &profile,
            42,
        );
        assert!(events.iter().any(|e| matches!(e, InputEvent::Wait(_))));
        assert!(events.iter().any(|e| matches!(e, InputEvent::Mouse(_))));
        assert!(events.iter().any(|e| matches!(e, InputEvent::Scroll(_))));
        assert!(events.iter().any(|e| matches!(e, InputEvent::Keyboard(_))));
    }
}
