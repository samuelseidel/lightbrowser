//! `speechSynthesis.speak()` and its utterances and events (ADR 0047).
//!
//! The control flow is Blink's `SpeechSynthesis` (one queue per window, the
//! front utterance current, `speaking` true while the queue is not empty,
//! `pending` true when it holds more than one, `cancel()` clearing it
//! silently) in front of Chrome's browser-side `TtsController` (one
//! utterance at a time, a `paused` flag of its own that only `pause()`
//! sets and only a `resume()` with an utterance queued clears). Recorded in
//! Chrome 153 on Linux, which has no voice (`tests/webplat/cases/speech_none`):
//!
//! - `speak()` without a user activation (sticky) fires `error` with
//!   `not-allowed` at once, inside the call, and queues nothing;
//! - with one and no voice, `error` with `synthesis-failed` follows in the
//!   next task, one utterance after the other (`speaking` is true during
//!   the first one's event: the second is already current);
//! - `pause()` with nothing to say leaves `paused` false but holds the next
//!   utterance in the controller, which `resume()` releases; a `resume()`
//!   with an empty queue does nothing (so `pause(); resume();` leaves the
//!   controller paused);
//! - `cancel()` fires `error` with `canceled` for the utterance the
//!   controller holds, in a later task, and nothing for the ones still
//!   queued.
//!
//! With a voice (the persona's `speech_voices`) there is nothing to record
//! on the reference machine, so the engine follows Blink's event order and a
//! documented duration model: `start`, one `boundary` event (`name`
//! `word`, `charIndex`, `charLength`) per word if the voice is a local
//! one (native voices send word boundaries, network voices of Chrome's
//! speech extension send none), then `end` (`charIndex` the text's length)
//! after `words * 400 ms / rate` (150 words per minute at rate 1). The
//! `pause` and `resume` events come in the task after the call, with the
//! last boundary's `charIndex`; `paused` follows them. `elapsedTime` is in
//! seconds since the utterance was handed to the controller, rounded to
//! `float`. All times come from the window's clock, so a virtual clock
//! works.

use std::collections::VecDeque;
use std::rc::Rc;

use lb_js::{Arg, CallCtx, HostError, HostRef};

use super::events::{EventExtra, fire_event_with};
use super::{
    Dict, Js, K_OBJ, Obj, Runtime, TaskSource, TimerCallback, Window, ctor, getter, setter,
};
use crate::registry::Registry;

/// A `SpeechSynthesisUtterance`.
pub(crate) struct UtteranceData {
    pub text: String,
    pub lang: String,
    /// Index into the persona's `speech_voices`.
    pub voice: Option<u16>,
    pub volume: f32,
    pub rate: f32,
    pub pitch: f32,
    /// Window clock (ms) when it was handed to the controller.
    start_ms: Option<f64>,
}

/// State of `SpeechSynthesisEvent` and `SpeechSynthesisErrorEvent`.
#[derive(Clone, Debug, Default)]
pub struct SpeechData {
    pub utterance: Option<HostRef>,
    pub char_index: u32,
    pub char_length: u32,
    pub elapsed: f32,
    pub name: String,
    /// `SpeechSynthesisErrorEvent.error` (empty for the other class).
    pub error: String,
}

#[derive(Clone, Debug)]
enum Kind {
    Start,
    Boundary { index: u32, len: u32 },
    End,
}

/// The browser side's playing of one utterance with a voice.
struct Play {
    /// `(offset in ms, event)` in order.
    script: Vec<(f64, Kind)>,
    /// The next event of `script`.
    idx: usize,
    /// Clock time (ms) of script offset 0 (moved by pauses).
    base: f64,
    /// Script position (ms) while paused.
    pos: f64,
    paused: bool,
    timer: Option<u32>,
    /// `charIndex` of the last word boundary.
    last_boundary: u32,
}

/// What the controller holds.
struct Ctl {
    utt: u32,
    /// Kept back by the controller's paused flag until `resume()`.
    held: bool,
    play: Option<Play>,
}

#[derive(Default)]
pub(crate) struct SpeechState {
    /// Blink's queue: the front is the current utterance.
    queue: VecDeque<u32>,
    /// Blink's `paused` (set by the `pause` event, cleared by `resume`).
    paused: bool,
    /// The controller's paused flag.
    ctl_paused: bool,
    /// Bumped whenever the controller's utterance changes: tasks and
    /// timers of an earlier one find it out and do nothing.
    epoch: u64,
    ctl: Option<Ctl>,
}

/// Milliseconds a word takes at rate 1 (150 words per minute).
pub(crate) const WORD_MS: f64 = 400.0;

const ERROR_CODES: &[&str] = &[
    "canceled",
    "interrupted",
    "audio-busy",
    "audio-hardware",
    "network",
    "synthesis-unavailable",
    "synthesis-failed",
    "language-unavailable",
    "voice-unavailable",
    "text-too-long",
    "invalid-argument",
    "not-allowed",
];

fn utterance<R>(w: &Window, id: u32, f: impl FnOnce(&mut UtteranceData) -> R) -> Option<R> {
    w.with_obj(id, |o| match o {
        Obj::Utterance(u) => Some(f(u)),
        _ => None,
    })
    .flatten()
}

fn this_utterance(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<u32, HostError> {
    let id = super::this_obj(cx)?;
    utterance(w, id, |_| ()).ok_or_else(super::illegal)?;
    Ok(id)
}

/// Fire a speech event at utterance `utt`, `elapsedTime` as of now.
fn fire(
    js: &mut dyn Js,
    w: &Rc<Window>,
    utt: u32,
    type_: &str,
    (char_index, char_length): (u32, u32),
    name: &str,
    error: Option<&str>,
) {
    let now = w.clock.now();
    let start = utterance(w, utt, |u| u.start_ms).flatten().unwrap_or(now);
    let data = SpeechData {
        utterance: Some(HostRef::new(K_OBJ, utt)),
        char_index,
        char_length,
        elapsed: ((now - start).max(0.0) / 1000.0) as f32,
        name: name.to_owned(),
        error: error.unwrap_or_default().to_owned(),
    };
    fire_event_with(
        js,
        w,
        HostRef::new(K_OBJ, utt),
        if error.is_some() {
            "SpeechSynthesisErrorEvent"
        } else {
            "SpeechSynthesisEvent"
        },
        type_,
        false,
        false,
        EventExtra::Speech(Box::new(data)),
    );
}

// ---------------------------------------------------------------------------
// The voice an utterance gets
// ---------------------------------------------------------------------------

/// The voice the controller picks: the utterance's own, else the first
/// whose language matches `lang` (the whole tag, else the primary
/// subtag), else the persona's default voice, else the first. `None` when
/// the persona has no voice.
fn choose_voice(w: &Window, utt: u32) -> Option<(u16, bool)> {
    let voices = &w.profile.as_deref()?.speech_voices;
    if voices.is_empty() {
        return None;
    }
    let (own, lang) = utterance(w, utt, |u| (u.voice, u.lang.clone()))?;
    let pick = own
        .map(usize::from)
        .filter(|&i| i < voices.len())
        .or_else(|| {
            let primary = |s: &str| {
                s.split(['-', '_'])
                    .next()
                    .unwrap_or("")
                    .to_ascii_lowercase()
            };
            let want = lang.replace('_', "-").to_ascii_lowercase();
            if want.is_empty() {
                return None;
            }
            voices
                .iter()
                .position(|v| v.lang.replace('_', "-").eq_ignore_ascii_case(&want))
                .or_else(|| {
                    voices
                        .iter()
                        .position(|v| primary(&v.lang) == primary(&want))
                })
        })
        .or_else(|| voices.iter().position(|v| v.default))
        .unwrap_or(0);
    Some((pick as u16, voices[pick].local_service))
}

/// The events of speaking `text` at `rate` with a voice that sends word
/// boundaries when `words` is set.
fn script(text: &str, rate: f64, words: bool) -> Vec<(f64, Kind)> {
    let per_word = WORD_MS / rate;
    let mut out = vec![(0.0, Kind::Start)];
    let mut n = 0u32;
    let (mut units, mut start, mut start_units) = (0u32, None::<usize>, 0u32);
    let mut flush = |end_byte: usize, start: &mut Option<usize>, start_units: u32, n: &mut u32| {
        if let Some(s) = start.take() {
            if words {
                let len = text[s..end_byte].encode_utf16().count() as u32;
                out.push((
                    f64::from(*n) * per_word,
                    Kind::Boundary {
                        index: start_units,
                        len,
                    },
                ));
            }
            *n += 1;
        }
    };
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            flush(i, &mut start, start_units, &mut n);
        } else if start.is_none() {
            start = Some(i);
            start_units = units;
        }
        units += c.len_utf16() as u32;
    }
    flush(text.len(), &mut start, start_units, &mut n);
    out.push((f64::from(n) * per_word, Kind::End));
    out
}

// ---------------------------------------------------------------------------
// Blink's side
// ---------------------------------------------------------------------------

/// `SpeechSynthesis.speak(utterance)` after its arguments were checked.
pub(crate) fn speak(js: &mut dyn Js, w: &Rc<Window>, utt: u32) {
    if !super::input::has_sticky_activation(w) {
        fire(js, w, utt, "error", (0, 0), "", Some("not-allowed"));
        return;
    }
    let first = {
        let mut st = w.speech.borrow_mut();
        st.queue.push_back(utt);
        st.queue.len() == 1
    };
    if first {
        start_front(w);
    }
}

pub(crate) fn speaking(w: &Window) -> bool {
    !w.speech.borrow().queue.is_empty()
}

pub(crate) fn pending(w: &Window) -> bool {
    w.speech.borrow().queue.len() > 1
}

pub(crate) fn paused(w: &Window) -> bool {
    w.speech.borrow().paused
}

/// Hand the current (front) utterance to the controller.
fn start_front(w: &Rc<Window>) {
    let Some(utt) = w.speech.borrow().queue.front().copied() else {
        return;
    };
    utterance(w, utt, |u| u.start_ms = Some(w.clock.now()));
    let (epoch, held) = {
        let mut st = w.speech.borrow_mut();
        st.epoch += 1;
        let held = st.ctl_paused;
        st.ctl = Some(Ctl {
            utt,
            held,
            play: None,
        });
        (st.epoch, held)
    };
    if !held {
        begin(w, utt, epoch);
    }
}

/// The controller starts the utterance it holds.
fn begin(w: &Rc<Window>, utt: u32, epoch: u64) {
    let Some((_, local)) = choose_voice(w, utt) else {
        // No voice: the controller answers with an error in a later task.
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, w| {
                if current(w, epoch, utt) {
                    drop_ctl(w);
                    complete(js, w, utt, Some("synthesis-failed"));
                }
            }),
        );
        return;
    };
    let (text, rate) =
        utterance(w, utt, |u| (u.text.clone(), f64::from(u.rate))).unwrap_or_default();
    let play = Play {
        script: script(&text, rate.max(0.1), local),
        idx: 0,
        base: w.clock.now(),
        pos: 0.0,
        paused: false,
        timer: None,
        last_boundary: 0,
    };
    {
        let mut st = w.speech.borrow_mut();
        if let Some(c) = st.ctl.as_mut() {
            c.held = false;
            c.play = Some(play);
        }
    }
    schedule_next(w, epoch);
}

fn current(w: &Window, epoch: u64, utt: u32) -> bool {
    let st = w.speech.borrow();
    st.epoch == epoch && st.ctl.as_ref().is_some_and(|c| c.utt == utt)
}

/// Forget the controller's utterance (its timer too); the epoch moves on.
fn drop_ctl(w: &Window) -> Option<u32> {
    let mut st = w.speech.borrow_mut();
    st.epoch += 1;
    let c = st.ctl.take()?;
    if let Some(t) = c.play.and_then(|p| p.timer) {
        w.timers.borrow_mut().remove(&t);
    }
    Some(c.utt)
}

/// Arrange for the next event of the playing script.
fn schedule_next(w: &Rc<Window>, epoch: u64) {
    let (delay, utt) = {
        let st = w.speech.borrow();
        if st.epoch != epoch {
            return;
        }
        let Some(c) = st.ctl.as_ref() else { return };
        let Some(p) = c
            .play
            .as_ref()
            .filter(|p| !p.paused && p.idx < p.script.len())
        else {
            return;
        };
        ((p.base + p.script[p.idx].0 - w.clock.now()).max(0.0), c.utt)
    };
    if delay <= 0.0 {
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, w| run_event(js, w, epoch, utt)),
        );
        return;
    }
    let id = w.add_timer(
        TimerCallback::Task(Some(Box::new(move |js, w| run_event(js, w, epoch, utt)))),
        delay,
        Vec::new(),
        false,
    );
    let mut st = w.speech.borrow_mut();
    if st.epoch == epoch
        && let Some(p) = st.ctl.as_mut().and_then(|c| c.play.as_mut())
    {
        p.timer = Some(id);
    }
}

/// The next event of the script is due.
fn run_event(js: &mut dyn Js, w: &Rc<Window>, epoch: u64, utt: u32) {
    if !current(w, epoch, utt) {
        return;
    }
    let (kind, text_len) = {
        let mut st = w.speech.borrow_mut();
        let Some(p) = st.ctl.as_mut().and_then(|c| c.play.as_mut()) else {
            return;
        };
        if p.paused || p.idx >= p.script.len() {
            return;
        }
        p.timer = None;
        let kind = p.script[p.idx].1.clone();
        p.idx += 1;
        if let Kind::Boundary { index, .. } = kind {
            p.last_boundary = index;
        }
        (
            kind,
            utterance(w, utt, |u| u.text.encode_utf16().count() as u32).unwrap_or(0),
        )
    };
    match kind {
        Kind::Start => fire(js, w, utt, "start", (0, 0), "", None),
        Kind::Boundary { index, len } => fire(js, w, utt, "boundary", (index, len), "word", None),
        Kind::End => {
            drop_ctl(w);
            complete_with(js, w, utt, text_len, None);
            return;
        }
    }
    schedule_next(w, epoch);
}

/// Blink's `HandleSpeakingCompleted`: the utterance leaves the queue if it
/// is the current one, its `end` or `error` event fires, then the next one
/// starts.
fn complete(js: &mut dyn Js, w: &Rc<Window>, utt: u32, error: Option<&str>) {
    complete_with(js, w, utt, 0, error);
}

fn complete_with(js: &mut dyn Js, w: &Rc<Window>, utt: u32, char_index: u32, error: Option<&str>) {
    let start_next = {
        let mut st = w.speech.borrow_mut();
        st.paused = false;
        if st.queue.front() == Some(&utt) {
            st.queue.pop_front();
            !st.queue.is_empty()
        } else {
            false
        }
    };
    match error {
        Some(e) => fire(js, w, utt, "error", (char_index, 0), "", Some(e)),
        None => fire(js, w, utt, "end", (char_index, 0), "", None),
    }
    if start_next && !w.speech.borrow().queue.is_empty() {
        start_front(w);
    }
}

/// `SpeechSynthesis.cancel()`.
pub(crate) fn cancel(w: &Rc<Window>) {
    {
        let mut st = w.speech.borrow_mut();
        st.queue.clear();
        st.ctl_paused = false;
    }
    if let Some(utt) = drop_ctl(w) {
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, w| complete(js, w, utt, Some("canceled"))),
        );
    }
}

/// `SpeechSynthesis.pause()`.
pub(crate) fn pause(w: &Rc<Window>) {
    if w.speech.borrow().paused {
        return;
    }
    let target = {
        let mut st = w.speech.borrow_mut();
        st.ctl_paused = true;
        let epoch = st.epoch;
        let now = w.clock.now();
        st.ctl.as_mut().and_then(|c| {
            let p = c.play.as_mut().filter(|p| !p.paused)?;
            p.paused = true;
            p.pos = now - p.base;
            if let Some(t) = p.timer.take() {
                w.timers.borrow_mut().remove(&t);
            }
            Some((c.utt, epoch))
        })
    };
    if let Some((utt, epoch)) = target {
        w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, w| {
                if !current(w, epoch, utt) {
                    return;
                }
                let at = {
                    let mut st = w.speech.borrow_mut();
                    st.paused = true;
                    st.ctl
                        .as_ref()
                        .and_then(|c| c.play.as_ref())
                        .map_or(0, |p| p.last_boundary)
                };
                fire(js, w, utt, "pause", (at, 0), "", None);
            }),
        );
    }
}

/// `SpeechSynthesis.resume()`.
pub(crate) fn resume(w: &Rc<Window>) {
    // Blink does nothing without a current utterance.
    if w.speech.borrow().queue.is_empty() {
        return;
    }
    enum Next {
        Release(u32, u64),
        Unpause(u32, u64),
        Nothing,
    }
    let next = {
        let mut st = w.speech.borrow_mut();
        st.ctl_paused = false;
        let epoch = st.epoch;
        let now = w.clock.now();
        match st.ctl.as_mut() {
            Some(c) if c.held => Next::Release(c.utt, epoch),
            Some(c) => match c.play.as_mut().filter(|p| p.paused) {
                Some(p) => {
                    p.base = now - p.pos;
                    p.paused = false;
                    Next::Unpause(c.utt, epoch)
                }
                None => Next::Nothing,
            },
            None => Next::Nothing,
        }
    };
    match next {
        Next::Release(utt, epoch) => begin(w, utt, epoch),
        Next::Unpause(utt, epoch) => w.queue_task(
            TaskSource::Networking,
            Box::new(move |js, w| {
                if !current(w, epoch, utt) {
                    return;
                }
                let at = {
                    let mut st = w.speech.borrow_mut();
                    st.paused = false;
                    st.ctl
                        .as_ref()
                        .and_then(|c| c.play.as_ref())
                        .map_or(0, |p| p.last_boundary)
                };
                fire(js, w, utt, "resume", (at, 0), "", None);
                schedule_next(w, epoch);
            }),
        ),
        Next::Nothing => {}
    }
}

// ---------------------------------------------------------------------------
// Bindings
// ---------------------------------------------------------------------------

fn f32_member(cx: &mut CallCtx<'_, '_, '_>, iface: &str, name: &str) -> Result<f32, HostError> {
    let v = cx.arg_f64(0)?;
    if !v.is_finite() {
        return Err(HostError::type_error(format!(
            "Failed to set the '{name}' property on '{iface}': The provided float value is non-finite."
        )));
    }
    Ok(v as f32)
}

/// A required `utterance` member of a `SpeechSynthesisEventInit`.
fn init_utterance(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    d: &Dict,
    iface: &str,
) -> Result<HostRef, HostError> {
    let pre = format!(
        "Failed to construct '{iface}': Failed to read the 'utterance' property from 'SpeechSynthesisEventInit': "
    );
    let Some(h) = d.get(cx, "utterance")? else {
        return Err(HostError::type_error(format!(
            "{pre}Required member is undefined."
        )));
    };
    let host = cx.handle_host(h);
    cx.release(h);
    match host {
        Some(r) if r.kind == K_OBJ && utterance(w, r.id, |_| ()).is_some() => Ok(r),
        _ => Err(HostError::type_error(format!(
            "{pre}Failed to convert value to 'SpeechSynthesisUtterance'."
        ))),
    }
}

fn speech_event_ctor(reg: &mut Registry, rt: &Runtime, iface: &'static str, errors: bool) {
    ctor(reg, rt, iface, move |cx, w| {
        if cx.len() < 2 {
            return Err(HostError::type_error(format!(
                "Failed to construct '{iface}': 2 arguments required, but only {} present.",
                cx.len()
            )));
        }
        let type_ = cx.arg_string(0)?;
        let d = Dict::arg(cx, 1);
        let build = |cx: &mut CallCtx<'_, '_, '_>| -> Result<SpeechData, HostError> {
            let utt = init_utterance(cx, w, &d, iface)?;
            let mut data = SpeechData {
                utterance: Some(utt),
                char_index: d.number(cx, "charIndex")?.map_or(0, |n| n as u32),
                char_length: d.number(cx, "charLength")?.map_or(0, |n| n as u32),
                elapsed: d.number(cx, "elapsedTime")?.map_or(0.0, |n| n as f32),
                name: d.string(cx, "name")?.unwrap_or_default(),
                error: String::new(),
            };
            if errors {
                let pre = format!(
                    "Failed to construct '{iface}': Failed to read the 'error' property from 'SpeechSynthesisErrorEventInit': "
                );
                match d.string(cx, "error")? {
                    None => {
                        return Err(HostError::type_error(format!(
                            "{pre}Required member is undefined."
                        )));
                    }
                    Some(e) if ERROR_CODES.contains(&e.as_str()) => data.error = e,
                    Some(e) => {
                        return Err(HostError::type_error(format!(
                            "{pre}The provided value '{e}' is not a valid enum value of type SpeechSynthesisErrorCode."
                        )));
                    }
                }
            }
            Ok(data)
        };
        let data = build(cx);
        d.release(cx);
        let mut e = super::events::EventData::new(iface, &type_, false, false);
        e.extra = EventExtra::Speech(Box::new(data?));
        e.time = super::events::coarse_now(w);
        let id = w.new_obj(Obj::Event(Box::new(e)));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    // SpeechSynthesisUtterance
    ctor(reg, rt, "SpeechSynthesisUtterance", |cx, w| {
        let text = if cx.is_empty() || cx.arg_kind(0) == lb_js::ValueKind::Undefined {
            String::new()
        } else {
            cx.arg_string(0)?
        };
        let id = w.new_obj(Obj::Utterance(Box::new(UtteranceData {
            text,
            lang: String::new(),
            voice: None,
            volume: 1.0,
            rate: 1.0,
            pitch: 1.0,
            start_ms: None,
        })));
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    macro_rules! string_attr {
        ($name:literal, $field:ident) => {
            getter(reg, rt, "SpeechSynthesisUtterance", $name, |cx, w| {
                let id = this_utterance(cx, w)?;
                let v = utterance(w, id, |u| u.$field.clone()).unwrap_or_default();
                cx.ret(v)
            });
            setter(reg, rt, "SpeechSynthesisUtterance", $name, |cx, w| {
                let id = this_utterance(cx, w)?;
                let v = cx.arg_string(0)?;
                utterance(w, id, |u| u.$field = v);
                Ok(())
            });
        };
    }
    string_attr!("text", text);
    string_attr!("lang", lang);
    // Chrome clamps: volume to [0, 1], rate to [0.1, 10], pitch to [0, 2];
    // all are `float`, so values read back rounded to 32 bits.
    macro_rules! float_attr {
        ($name:literal, $field:ident, $lo:expr, $hi:expr) => {
            getter(reg, rt, "SpeechSynthesisUtterance", $name, |cx, w| {
                let id = this_utterance(cx, w)?;
                let v = utterance(w, id, |u| u.$field).unwrap_or(1.0);
                cx.ret(f64::from(v))
            });
            setter(reg, rt, "SpeechSynthesisUtterance", $name, |cx, w| {
                let id = this_utterance(cx, w)?;
                let v = f32_member(cx, "SpeechSynthesisUtterance", $name)?;
                utterance(w, id, |u| u.$field = v.clamp($lo, $hi));
                Ok(())
            });
        };
    }
    float_attr!("volume", volume, 0.0, 1.0);
    float_attr!("rate", rate, 0.1, 10.0);
    float_attr!("pitch", pitch, 0.0, 2.0);
    getter(reg, rt, "SpeechSynthesisUtterance", "voice", |cx, w| {
        let id = this_utterance(cx, w)?;
        match utterance(w, id, |u| u.voice).flatten() {
            Some(i) => cx.ret(super::navigator::voice_arg(w, i)),
            None => cx.ret(Arg::Null),
        }
    });
    setter(reg, rt, "SpeechSynthesisUtterance", "voice", |cx, w| {
        let id = this_utterance(cx, w)?;
        let v = if cx.arg_kind(0) == lb_js::ValueKind::Null {
            None
        } else {
            let idx = cx
                .arg_host(0)
                .filter(|h| h.kind == K_OBJ)
                .and_then(|h| super::navigator::voice_index(w, h.id));
            match idx {
                Some(i) => Some(i),
                None => {
                    return Err(HostError::type_error(
                        "Failed to set the 'voice' property on 'SpeechSynthesisUtterance': Failed to convert value to 'SpeechSynthesisVoice'.",
                    ));
                }
            }
        };
        utterance(w, id, |u| u.voice = v);
        Ok(())
    });

    // SpeechSynthesisEvent, SpeechSynthesisErrorEvent
    speech_event_ctor(reg, rt, "SpeechSynthesisEvent", false);
    speech_event_ctor(reg, rt, "SpeechSynthesisErrorEvent", true);
    fn data<R>(
        cx: &mut CallCtx<'_, '_, '_>,
        w: &Window,
        f: impl FnOnce(&SpeechData) -> R,
    ) -> Result<R, HostError> {
        let id = super::this_obj(cx)?;
        w.with_obj(id, |o| match o {
            Obj::Event(e) => match &e.extra {
                EventExtra::Speech(d) => Some(f(d)),
                _ => None,
            },
            _ => None,
        })
        .flatten()
        .ok_or_else(super::illegal)
    }
    getter(reg, rt, "SpeechSynthesisEvent", "utterance", |cx, w| {
        let u = data(cx, w, |d| d.utterance)?;
        cx.ret(u.map_or(Arg::Null, |r| w.obj_arg(r.id)))
    });
    getter(reg, rt, "SpeechSynthesisEvent", "charIndex", |cx, w| {
        let v = data(cx, w, |d| d.char_index)?;
        cx.ret(f64::from(v))
    });
    getter(reg, rt, "SpeechSynthesisEvent", "charLength", |cx, w| {
        let v = data(cx, w, |d| d.char_length)?;
        cx.ret(f64::from(v))
    });
    getter(reg, rt, "SpeechSynthesisEvent", "elapsedTime", |cx, w| {
        let v = data(cx, w, |d| d.elapsed)?;
        cx.ret(f64::from(v))
    });
    getter(reg, rt, "SpeechSynthesisEvent", "name", |cx, w| {
        let v = data(cx, w, |d| d.name.clone())?;
        cx.ret(v)
    });
    getter(reg, rt, "SpeechSynthesisErrorEvent", "error", |cx, w| {
        let v = data(cx, w, |d| d.error.clone())?;
        cx.ret(v)
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The duration model: 400 ms a word at rate 1, boundaries only for
    /// local voices, UTF-16 indices (no browser to record: this is the
    /// documented model of the module header).
    #[test]
    fn script_of_words() {
        let s = script("ab  \u{1F600}x c", 2.0, true);
        let at: Vec<(f64, String)> = s.iter().map(|(t, k)| (*t, format!("{k:?}"))).collect();
        assert_eq!(at[0], (0.0, "Start".into()));
        assert_eq!(at[1], (0.0, "Boundary { index: 0, len: 2 }".into()));
        // The emoji is two UTF-16 units: the word starts at 4, is 3 long.
        assert_eq!(at[2], (200.0, "Boundary { index: 4, len: 3 }".into()));
        assert_eq!(at[3], (400.0, "Boundary { index: 8, len: 1 }".into()));
        assert_eq!(at[4], (600.0, "End".into()));
        assert_eq!(script("a b", 1.0, false).len(), 2);
        assert_eq!(script("", 1.0, true).len(), 2);
    }
}
