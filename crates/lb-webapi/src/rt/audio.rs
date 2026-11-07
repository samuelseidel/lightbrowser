//! Web Audio on [`super::audio_dsp`]: `AudioContext`, `OfflineAudioContext`,
//! the common nodes, `AudioParam`, `AudioBuffer`, `PeriodicWave`.
//!
//! - Sample rate, `baseLatency` and `destination.maxChannelCount` of a
//!   realtime context come from the persona (`audio`), else 48 kHz on
//!   Windows and 44.1 kHz elsewhere. A new `AudioContext` starts
//!   `suspended` (Chrome's autoplay policy without a user gesture);
//!   `resume()` makes it run on the page clock, and its graph is rendered
//!   lazily when `currentTime` or an analyser is read.
//! - `OfflineAudioContext.startRendering()` renders synchronously, then
//!   resolves the promise and fires `complete` from tasks, like Chrome.
//!   The rendered channels carry the persona's audio noise
//!   ([`super::audio_dsp::apply_noise`], stream = channel index); analyser
//!   float outputs carry it too. Silence stays exact.
//! - Kernels: oscillator, gain, dynamics compressor, analyser, biquad,
//!   IIR, buffer source, constant source, delay, stereo panner, wave
//!   shaper, channel merger/splitter. Panner, convolver and script
//!   processor pass their input through (no `audioprocess` events).
//!   `decodeAudioData` rejects (no codecs).

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use lb_js::{Arg, CallCtx, Handle, HostError, HostRef, JsValue, ValueKind, ViewKind};

use super::audio_dsp::{
    self as dsp, Analyser, BiquadType, CountMode, Event, Graph, Kind, Node, Param, PeriodicWave,
    Schedule, Shape,
};
use super::{
    Dict, K_OBJ, Obj, Runtime, TaskSource, Window, ctor, getter, illegal, method, need_args,
    setter, this_obj,
};
use crate::registry::Registry;

const F32_MAX: f32 = f32::MAX;
/// `1200 * log2(FLT_MAX)`, Blink's detune range.
const DETUNE_MAX: f32 = 153_600.0;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// A stored node attribute without its own kernel field.
#[derive(Clone, Debug)]
pub(crate) enum Prop {
    Num(f64),
    Str(String),
    Bool(bool),
    Handle(Handle),
}

/// One audio context.
pub(crate) struct Ctx {
    offline: bool,
    pub(crate) graph: Graph,
    length: usize,
    state: &'static str,
    obj: u32,
    /// Node index to object id (0 is the destination).
    nodes: Vec<u32>,
    listener: Option<u32>,
    listener_node: Option<usize>,
    started: bool,
    /// Buffer source node index to `AudioBuffer` object id.
    buffers: HashMap<usize, u32>,
    props: HashMap<(usize, &'static str), Prop>,
    /// Page clock (ms) when the context started running, and the frame
    /// it had rendered then.
    run_start: Option<(f64, u64)>,
    base_latency: f64,
    /// `outputLatency` while running.
    output_latency: f64,
    max_channels: u32,
}

/// `AudioBuffer` contents. Once `getChannelData` handed out a
/// `Float32Array` for a channel, that array is the channel's storage.
pub(crate) struct Buf {
    sample_rate: f32,
    length: usize,
    data: Vec<Vec<f32>>,
    views: Vec<Option<Handle>>,
}

/// Audio host objects (one `Obj` variant).
#[derive(Clone)]
pub(crate) enum AudioObj {
    Context {
        ctx: Rc<RefCell<Ctx>>,
        offline: bool,
    },
    Node {
        ctx: Rc<RefCell<Ctx>>,
        node: usize,
        iface: &'static str,
    },
    Param {
        ctx: Rc<RefCell<Ctx>>,
        node: usize,
        param: usize,
    },
    Listener {
        ctx: Rc<RefCell<Ctx>>,
    },
    Buffer(Rc<RefCell<Buf>>),
    Wave(Rc<PeriodicWave>),
}

impl AudioObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            AudioObj::Context { offline: true, .. } => "OfflineAudioContext",
            AudioObj::Context { .. } => "AudioContext",
            AudioObj::Node { iface, .. } => iface,
            AudioObj::Param { .. } => "AudioParam",
            AudioObj::Listener { .. } => "AudioListener",
            AudioObj::Buffer(_) => "AudioBuffer",
            AudioObj::Wave(_) => "PeriodicWave",
        }
    }
}

fn get_a(w: &Window, id: u32) -> Option<AudioObj> {
    w.with_obj(id, |o| match o {
        Obj::Audio(a) => Some((**a).clone()),
        _ => None,
    })
    .flatten()
}

fn new_a(w: &Window, a: AudioObj) -> u32 {
    w.new_obj(Obj::Audio(Box::new(a)))
}

fn this_a(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<AudioObj, HostError> {
    let id = this_obj(cx)?;
    get_a(w, id).ok_or_else(illegal)
}

fn arg_a(cx: &mut CallCtx<'_, '_, '_>, w: &Window, i: usize) -> Option<(u32, AudioObj)> {
    let h = cx.arg_host(i).filter(|h| h.kind == K_OBJ)?;
    get_a(w, h.id).map(|a| (h.id, a))
}

fn this_ctx(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Rc<RefCell<Ctx>>, HostError> {
    match this_a(cx, w)? {
        AudioObj::Context { ctx, .. } => Ok(ctx),
        _ => Err(illegal()),
    }
}

fn this_node(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
) -> Result<(Rc<RefCell<Ctx>>, usize), HostError> {
    match this_a(cx, w)? {
        AudioObj::Node { ctx, node, .. } => Ok((ctx, node)),
        _ => Err(illegal()),
    }
}

fn seed(w: &Window) -> Option<[u8; 16]> {
    w.profile
        .as_ref()
        .and_then(|p| p.fingerprint_seeds.audio_bytes())
}

fn f32s_to_bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_ne_bytes()).collect()
}

fn bytes_to_f32s(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_ne_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

/// A `sequence<float>` or `Float32Array` argument.
fn arg_floats(cx: &mut CallCtx<'_, '_, '_>, i: usize) -> Option<Vec<f32>> {
    if i >= cx.len() {
        return None;
    }
    match cx.arg_view_kind(i) {
        Some(ViewKind::Float32) => return cx.arg_bytes(i).map(|b| bytes_to_f32s(&b)),
        Some(ViewKind::Float64) => {
            return cx.arg_bytes(i).map(|b| {
                b.chunks_exact(8)
                    .map(|c| {
                        f64::from_ne_bytes([c[0], c[1], c[2], c[3], c[4], c[5], c[6], c[7]]) as f32
                    })
                    .collect()
            });
        }
        Some(_) => return None,
        None => {}
    }
    match cx.arg_value(i) {
        JsValue::Json(s) => serde_json::from_str::<Vec<serde_json::Value>>(&s)
            .ok()
            .map(|v| {
                v.iter()
                    .map(|x| x.as_f64().unwrap_or(f64::NAN) as f32)
                    .collect()
            }),
        _ => None,
    }
}

/// Returns `this` (chainable `AudioParam` methods).
fn ret_this(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(), HostError> {
    let id = this_obj(cx)?;
    cx.ret(w.obj_arg(id))
}

fn type_err(op: &str, iface: &str, msg: &str) -> HostError {
    HostError::type_error(format!("Failed to execute '{op}' on '{iface}': {msg}"))
}

// ---------------------------------------------------------------------------
// Node catalogue
// ---------------------------------------------------------------------------

/// `(attribute, default, min, max, a-rate)`; `None` bounds mean the
/// Nyquist frequency.
type ParamSpec = (&'static str, f32, Option<f32>, Option<f32>, bool);

struct NodeSpec {
    iface: &'static str,
    inputs: u32,
    outputs: u32,
    count: usize,
    mode: CountMode,
    params: &'static [ParamSpec],
}

const NO_PARAMS: &[ParamSpec] = &[];

fn spec(iface: &str) -> Option<NodeSpec> {
    use CountMode::*;
    fn s(
        iface: &'static str,
        inputs: u32,
        outputs: u32,
        count: usize,
        mode: CountMode,
        params: &'static [ParamSpec],
    ) -> NodeSpec {
        NodeSpec {
            iface,
            inputs,
            outputs,
            count,
            mode,
            params,
        }
    }
    Some(match iface {
        "OscillatorNode" => s(
            "OscillatorNode",
            0,
            1,
            2,
            Max,
            &[
                ("frequency", 440.0, None, None, true),
                ("detune", 0.0, Some(-DETUNE_MAX), Some(DETUNE_MAX), true),
            ],
        ),
        "GainNode" => s(
            "GainNode",
            1,
            1,
            2,
            Max,
            &[("gain", 1.0, Some(-F32_MAX), Some(F32_MAX), true)],
        ),
        "DynamicsCompressorNode" => s(
            "DynamicsCompressorNode",
            1,
            1,
            2,
            ClampedMax,
            &[
                ("threshold", -24.0, Some(-100.0), Some(0.0), false),
                ("knee", 30.0, Some(0.0), Some(40.0), false),
                ("ratio", 12.0, Some(1.0), Some(20.0), false),
                ("attack", 0.003, Some(0.0), Some(1.0), false),
                ("release", 0.25, Some(0.0), Some(1.0), false),
            ],
        ),
        "AnalyserNode" => s("AnalyserNode", 1, 1, 2, Max, NO_PARAMS),
        "BiquadFilterNode" => s(
            "BiquadFilterNode",
            1,
            1,
            2,
            Max,
            &[
                ("frequency", 350.0, Some(0.0), None, true),
                ("detune", 0.0, Some(-DETUNE_MAX), Some(DETUNE_MAX), true),
                ("Q", 1.0, Some(-F32_MAX), Some(F32_MAX), true),
                ("gain", 0.0, Some(-F32_MAX), Some(1541.2737), true),
            ],
        ),
        "IIRFilterNode" => s("IIRFilterNode", 1, 1, 2, Max, NO_PARAMS),
        "AudioBufferSourceNode" => s(
            "AudioBufferSourceNode",
            0,
            1,
            2,
            Max,
            &[
                ("playbackRate", 1.0, Some(-F32_MAX), Some(F32_MAX), false),
                ("detune", 0.0, Some(-F32_MAX), Some(F32_MAX), false),
            ],
        ),
        "ConstantSourceNode" => s(
            "ConstantSourceNode",
            0,
            1,
            2,
            Max,
            &[("offset", 1.0, Some(-F32_MAX), Some(F32_MAX), true)],
        ),
        "DelayNode" => s(
            "DelayNode",
            1,
            1,
            2,
            Max,
            &[("delayTime", 0.0, Some(0.0), Some(1.0), true)],
        ),
        "StereoPannerNode" => s(
            "StereoPannerNode",
            1,
            1,
            2,
            ClampedMax,
            &[("pan", 0.0, Some(-1.0), Some(1.0), true)],
        ),
        "WaveShaperNode" => s("WaveShaperNode", 1, 1, 2, Max, NO_PARAMS),
        "ChannelMergerNode" => s("ChannelMergerNode", 6, 1, 1, Explicit, NO_PARAMS),
        "ChannelSplitterNode" => s("ChannelSplitterNode", 1, 6, 6, Explicit, NO_PARAMS),
        "PannerNode" => s(
            "PannerNode",
            1,
            1,
            2,
            ClampedMax,
            &[
                ("positionX", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
                ("positionY", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
                ("positionZ", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
                ("orientationX", 1.0, Some(-F32_MAX), Some(F32_MAX), true),
                ("orientationY", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
                ("orientationZ", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
            ],
        ),
        "ConvolverNode" => s("ConvolverNode", 1, 1, 2, ClampedMax, NO_PARAMS),
        "ScriptProcessorNode" => s("ScriptProcessorNode", 1, 1, 2, Explicit, NO_PARAMS),
        _ => return None,
    })
}

const LISTENER_PARAMS: &[ParamSpec] = &[
    ("positionX", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("positionY", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("positionZ", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("forwardX", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("forwardY", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("forwardZ", -1.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("upX", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("upY", 1.0, Some(-F32_MAX), Some(F32_MAX), true),
    ("upZ", 0.0, Some(-F32_MAX), Some(F32_MAX), true),
];

fn make_params(specs: &[ParamSpec], sr: f32) -> Vec<Param> {
    let ny = sr / 2.0;
    specs
        .iter()
        .map(|(_, d, lo, hi, a)| Param::new(*d, lo.unwrap_or(-ny), hi.unwrap_or(ny), *a))
        .collect()
}

fn initial_kind(iface: &str, sr: f32) -> Kind {
    match iface {
        "OscillatorNode" => Kind::Oscillator {
            shape: Shape::Sine,
            wave: None,
            custom: None,
            phase: 0.0,
            sched: Schedule::default(),
        },
        "GainNode" => Kind::Gain,
        "DynamicsCompressorNode" => Kind::Compressor {
            kernel: Box::new(dsp::Compressor::new(sr, 2)),
            reduction: 0.0,
        },
        "AnalyserNode" => Kind::Analyser(Box::default()),
        "BiquadFilterNode" => Kind::Biquad {
            ty: BiquadType::Lowpass,
            state: Vec::new(),
        },
        "IIRFilterNode" => Kind::Iir {
            ff: vec![1.0],
            fb: vec![1.0],
            state: Vec::new(),
        },
        "AudioBufferSourceNode" => Kind::BufferSource {
            buffer: None,
            buffer_rate: sr,
            pos: 0.0,
            looping: false,
            loop_start: 0.0,
            loop_end: 0.0,
            offset: 0.0,
            sched: Schedule::default(),
        },
        "ConstantSourceNode" => Kind::Constant {
            sched: Schedule::default(),
        },
        "DelayNode" => Kind::Delay {
            max: 1.0,
            lines: Vec::new(),
            write: 0,
        },
        "StereoPannerNode" => Kind::StereoPanner,
        "WaveShaperNode" => Kind::WaveShaper { curve: None },
        "ChannelMergerNode" => Kind::Merger(6),
        "ChannelSplitterNode" => Kind::Splitter(6),
        "PannerNode" => Kind::PassThrough {
            out_channels: Some(2),
        },
        _ => Kind::PassThrough { out_channels: None },
    }
}

/// Creates node `iface` in `ctx`; returns its object id.
fn create_node(w: &Window, ctx: &Rc<RefCell<Ctx>>, iface: &'static str) -> u32 {
    let sp = spec(iface).expect("known node");
    debug_assert_eq!(sp.iface, iface);
    let idx = {
        let mut c = ctx.borrow_mut();
        let sr = c.graph.sample_rate;
        let node = Node::new(
            initial_kind(iface, sr),
            make_params(sp.params, sr),
            sp.count,
            sp.mode,
        );
        let idx = c.graph.add(node);
        if iface == "ChannelSplitterNode" {
            c.graph.nodes[idx].discrete = true;
        }
        idx
    };
    let id = new_a(
        w,
        AudioObj::Node {
            ctx: ctx.clone(),
            node: idx,
            iface,
        },
    );
    let mut c = ctx.borrow_mut();
    if c.nodes.len() <= idx {
        c.nodes.resize(idx + 1, u32::MAX);
    }
    c.nodes[idx] = id;
    id
}

fn new_context(
    w: &Window,
    offline: bool,
    sample_rate: f32,
    channels: usize,
    length: usize,
) -> (Rc<RefCell<Ctx>>, u32) {
    let defaults = audio_defaults(w);
    let ctx = Rc::new(RefCell::new(Ctx {
        offline,
        graph: Graph::new(sample_rate, channels),
        length,
        state: "suspended",
        obj: 0,
        nodes: vec![0],
        listener: None,
        listener_node: None,
        started: false,
        buffers: HashMap::new(),
        props: HashMap::new(),
        run_start: None,
        base_latency: defaults.base_latency,
        output_latency: defaults.output_latency,
        max_channels: if offline {
            channels as u32
        } else {
            defaults.channels
        },
    }));
    let id = new_a(
        w,
        AudioObj::Context {
            ctx: ctx.clone(),
            offline,
        },
    );
    let dest = new_a(
        w,
        AudioObj::Node {
            ctx: ctx.clone(),
            node: 0,
            iface: "AudioDestinationNode",
        },
    );
    {
        let mut c = ctx.borrow_mut();
        c.obj = id;
        c.nodes[0] = dest;
    }
    (ctx, id)
}

/// Realtime defaults from the persona's `audio_defaults()` (its `audio`,
/// else the OS default); without a persona 44.1 kHz, 10 ms base latency.
fn audio_defaults(w: &Window) -> lb_persona::AudioDefaults {
    w.profile.as_deref().map_or(
        lb_persona::AudioDefaults {
            sample_rate: 44_100,
            base_latency: 0.01,
            output_latency: 0.02,
            channels: 2,
        },
        lb_persona::PersonaProfile::audio_defaults,
    )
}

/// Default realtime sample rate.
fn default_rate(w: &Window) -> f32 {
    audio_defaults(w).sample_rate as f32
}

/// Advance a running realtime context's graph to the page clock.
fn sync(w: &Window, ctx: &Rc<RefCell<Ctx>>) {
    let mut c = ctx.borrow_mut();
    if c.offline || c.state != "running" {
        return;
    }
    let Some((start, base)) = c.run_start else {
        return;
    };
    let sr = f64::from(c.graph.sample_rate);
    let q = dsp::QUANTUM as u64;
    let elapsed = ((w.clock.now() - start).max(0.0) / 1000.0 * sr) as u64;
    let target = base + elapsed / q * q;
    if target <= c.graph.frame {
        return;
    }
    let max = (5.0 * sr) as u64 / q * q;
    if target - c.graph.frame > max {
        c.graph.frame = target - max;
    }
    while c.graph.frame < target {
        c.graph.render_quantum();
    }
}

// ---------------------------------------------------------------------------
// Buffers
// ---------------------------------------------------------------------------

fn new_buffer(w: &Window, sr: f32, length: usize, data: Vec<Vec<f32>>) -> u32 {
    let ch = data.len();
    new_a(
        w,
        AudioObj::Buffer(Rc::new(RefCell::new(Buf {
            sample_rate: sr,
            length,
            data,
            views: vec![None; ch],
        }))),
    )
}

/// Current samples of every channel (views win).
fn buffer_data(cx: &mut CallCtx<'_, '_, '_>, b: &Rc<RefCell<Buf>>) -> Vec<Vec<f32>> {
    let (views, mut data) = {
        let b = b.borrow();
        (b.views.clone(), b.data.clone())
    };
    for (c, v) in views.iter().enumerate() {
        if let Some(h) = v
            && let Some(bytes) = cx.handle_bytes(*h)
        {
            data[c] = bytes_to_f32s(&bytes);
        }
    }
    data
}

fn buffer_error(w: &Window, op: &str, channels: f64, length: f64, sr: f64) -> Option<HostError> {
    let iface = if op == "AudioBuffer" {
        format!("Failed to construct '{op}'")
    } else {
        format!("Failed to execute '{op}' on 'BaseAudioContext'")
    };
    if !(1.0..=32.0).contains(&channels) {
        return Some(w.rt.err(
            "NotSupportedError",
            format!(
                "{iface}: The number of channels provided ({channels}) is outside the range [1, 32]."
            ),
        ));
    }
    if length < 1.0 {
        return Some(w.rt.err(
            "NotSupportedError",
            format!("{iface}: The number of frames provided ({length}) is less than or equal to the minimum bound (0)."),
        ));
    }
    if !(3000.0..=768_000.0).contains(&sr) {
        return Some(w.rt.err(
            "NotSupportedError",
            format!(
                "{iface}: The sample rate provided ({sr}) is outside the range [3000, 768000]."
            ),
        ));
    }
    None
}

// ---------------------------------------------------------------------------
// Registration
// ---------------------------------------------------------------------------

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    register_contexts(reg, rt);
    register_factories(reg, rt);
    register_node_base(reg, rt);
    register_params(reg, rt);
    register_nodes(reg, rt);
    register_buffer(reg, rt);
}

fn register_contexts(reg: &mut Registry, rt: &Runtime) {
    ctor(reg, rt, "AudioContext", |cx, w| {
        let d = Dict::arg(cx, 0);
        let sr = d.number(cx, "sampleRate")?;
        d.release(cx);
        let sr = match sr {
            Some(r) if !(3000.0..=768_000.0).contains(&r) => {
                return Err(w.rt.err(
                    "NotSupportedError",
                    format!("Failed to construct 'AudioContext': The hardware sample rate provided ({r}) is outside the range [3000, 768000]."),
                ));
            }
            Some(r) => r as f32,
            None => default_rate(w),
        };
        let (_, id) = new_context(w, false, sr, 2, 0);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    ctor(reg, rt, "OfflineAudioContext", |cx, w| {
        let (ch, len, sr) = if !cx.is_empty() && matches!(cx.arg_kind(0), ValueKind::Object) {
            let d = Dict::arg(cx, 0);
            let ch = d.number(cx, "numberOfChannels")?.unwrap_or(1.0);
            let len = d.number(cx, "length")?;
            let sr = d.number(cx, "sampleRate")?;
            d.release(cx);
            let Some(len) = len else {
                return Err(HostError::type_error(
                    "Failed to construct 'OfflineAudioContext': Failed to read the 'length' property from 'OfflineAudioContextOptions': Required member is undefined.",
                ));
            };
            let Some(sr) = sr else {
                return Err(HostError::type_error(
                    "Failed to construct 'OfflineAudioContext': Failed to read the 'sampleRate' property from 'OfflineAudioContextOptions': Required member is undefined.",
                ));
            };
            (ch, len, sr)
        } else {
            need_args(cx, 3, "OfflineAudioContext", "OfflineAudioContext").map_err(|_| {
                HostError::type_error(format!(
                    "Failed to construct 'OfflineAudioContext': 3 arguments required, but only {} present.",
                    cx.len()
                ))
            })?;
            (cx.arg_f64(0)?, cx.arg_f64(1)?, cx.arg_f64(2)?)
        };
        let ch = ch.trunc();
        let len = len.trunc();
        if let Some(e) = buffer_error(w, "OfflineAudioContext", ch, len, sr) {
            return Err(e);
        }
        let (_, id) = new_context(w, true, sr as f32, ch as usize, len as usize);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    for iface in ["BaseAudioContext", "AudioContext", "OfflineAudioContext"] {
        getter(reg, rt, iface, "sampleRate", |cx, w| {
            let c = this_ctx(cx, w)?;
            let sr = c.borrow().graph.sample_rate;
            cx.ret(f64::from(sr))
        });
        getter(reg, rt, iface, "currentTime", |cx, w| {
            let c = this_ctx(cx, w)?;
            sync(w, &c);
            let t = c.borrow().graph.time();
            cx.ret(t)
        });
        getter(reg, rt, iface, "state", |cx, w| {
            let c = this_ctx(cx, w)?;
            let s = c.borrow().state;
            cx.ret(s)
        });
        getter(reg, rt, iface, "destination", |cx, w| {
            let c = this_ctx(cx, w)?;
            let id = c.borrow().nodes[0];
            cx.ret(w.obj_arg(id))
        });
        getter(reg, rt, iface, "renderQuantumSize", |cx, _w| {
            cx.ret(dsp::QUANTUM as u32)
        });
        getter(reg, rt, iface, "listener", |cx, w| {
            let c = this_ctx(cx, w)?;
            let have = c.borrow().listener;
            let id = match have {
                Some(id) => id,
                None => {
                    let idx = {
                        let mut cc = c.borrow_mut();
                        let sr = cc.graph.sample_rate;
                        cc.graph.add(Node::new(
                            Kind::PassThrough { out_channels: None },
                            make_params(LISTENER_PARAMS, sr),
                            2,
                            CountMode::Max,
                        ))
                    };
                    let id = new_a(w, AudioObj::Listener { ctx: c.clone() });
                    let mut cc = c.borrow_mut();
                    cc.listener = Some(id);
                    cc.listener_node = Some(idx);
                    if cc.nodes.len() <= idx {
                        cc.nodes.resize(idx + 1, u32::MAX);
                    }
                    cc.nodes[idx] = id;
                    id
                }
            };
            cx.ret(w.obj_arg(id))
        });
    }
    // AudioContext
    const AC: &str = "AudioContext";
    getter(reg, rt, AC, "baseLatency", |cx, w| {
        let c = this_ctx(cx, w)?;
        let v = c.borrow().base_latency;
        cx.ret(v)
    });
    getter(reg, rt, AC, "outputLatency", |cx, w| {
        let c = this_ctx(cx, w)?;
        let c = c.borrow();
        cx.ret(if c.state == "running" {
            c.output_latency
        } else {
            0.0
        })
    });
    getter(reg, rt, AC, "sinkId", |cx, _w| cx.ret(""));
    method(reg, rt, AC, "getOutputTimestamp", |cx, w| {
        let c = this_ctx(cx, w)?;
        sync(w, &c);
        let t = c.borrow().graph.time();
        let now = super::events::coarse_now(w);
        cx.ret(Arg::Json(format!(
            "{{\"contextTime\":{t},\"performanceTime\":{now}}}"
        )))
    });
    method(reg, rt, AC, "resume", |cx, w| {
        let c = this_ctx(cx, w)?;
        let r = cx.ret_new_promise()?;
        let (changed, obj) = {
            let mut cc = c.borrow_mut();
            if cc.state == "closed" {
                drop(cc);
                return cx.reject_promise(
                    r,
                    w.rt.err(
                        "InvalidStateError",
                        "Failed to execute 'resume' on 'AudioContext': Cannot resume a closed AudioContext.",
                    ),
                );
            }
            let changed = cc.state != "running";
            if changed {
                cc.state = "running";
                let f = cc.graph.frame;
                cc.run_start = Some((w.clock.now(), f));
            }
            (changed, cc.obj)
        };
        settle_with_statechange(w, r, obj, changed);
        Ok(())
    });
    method(reg, rt, AC, "suspend", |cx, w| {
        let c = this_ctx(cx, w)?;
        sync(w, &c);
        let r = cx.ret_new_promise()?;
        let (changed, obj) = {
            let mut cc = c.borrow_mut();
            if cc.state == "closed" {
                drop(cc);
                return cx.reject_promise(
                    r,
                    w.rt.err(
                        "InvalidStateError",
                        "Failed to execute 'suspend' on 'AudioContext': Cannot suspend a closed AudioContext.",
                    ),
                );
            }
            let changed = cc.state != "suspended";
            cc.state = "suspended";
            cc.run_start = None;
            (changed, cc.obj)
        };
        settle_with_statechange(w, r, obj, changed);
        Ok(())
    });
    method(reg, rt, AC, "close", |cx, w| {
        let c = this_ctx(cx, w)?;
        sync(w, &c);
        let r = cx.ret_new_promise()?;
        let (changed, obj) = {
            let mut cc = c.borrow_mut();
            let changed = cc.state != "closed";
            cc.state = "closed";
            cc.run_start = None;
            (changed, cc.obj)
        };
        settle_with_statechange(w, r, obj, changed);
        Ok(())
    });
    // OfflineAudioContext
    const OAC: &str = "OfflineAudioContext";
    getter(reg, rt, OAC, "length", |cx, w| {
        let c = this_ctx(cx, w)?;
        let l = c.borrow().length;
        cx.ret(l as f64)
    });
    method(reg, rt, OAC, "startRendering", |cx, w| {
        let c = this_ctx(cx, w)?;
        let r = cx.ret_new_promise()?;
        if c.borrow().started {
            return cx.reject_promise(
                r,
                w.rt.err(
                    "InvalidStateError",
                    "Failed to execute 'startRendering' on 'OfflineAudioContext': cannot call startRendering more than once",
                ),
            );
        }
        refresh_buffer_sources(cx, w, &c);
        let (out, sr, len, ended, obj) = {
            let mut cc = c.borrow_mut();
            cc.started = true;
            cc.state = "running";
            let len = cc.length;
            let mut out = cc.graph.render_frames(len);
            if let Some(s) = seed(w) {
                for (ch, d) in out.iter_mut().enumerate() {
                    dsp::apply_noise(s, ch as u32, d);
                }
            }
            let ended: Vec<u32> = cc
                .graph
                .take_ended()
                .into_iter()
                .filter_map(|n| cc.nodes.get(n).copied())
                .collect();
            cc.state = "closed";
            (out, cc.graph.sample_rate, len, ended, cc.obj)
        };
        let buf = new_buffer(w, sr, len, out);
        w.queue_task(
            TaskSource::DomManipulation,
            Box::new(move |js, w| {
                for n in ended {
                    super::fire_event(js, w, HostRef::new(K_OBJ, n), "ended", false, false);
                }
                js.resolve(r, w.obj_arg(buf));
            }),
        );
        w.queue_task(
            TaskSource::DomManipulation,
            Box::new(move |js, w| {
                let h = js.make(w.obj_arg(buf));
                super::fire_event_with(
                    js,
                    w,
                    HostRef::new(K_OBJ, obj),
                    "OfflineAudioCompletionEvent",
                    "complete",
                    false,
                    false,
                    super::EventExtra::Custom(h),
                );
            }),
        );
        Ok(())
    });
    method(reg, rt, OAC, "resume", |cx, w| {
        let c = this_ctx(cx, w)?;
        let r = cx.ret_new_promise()?;
        if !c.borrow().started {
            return cx.reject_promise(
                r,
                w.rt.err(
                    "InvalidStateError",
                    "Failed to execute 'resume' on 'OfflineAudioContext': cannot resume an offline context that has not started",
                ),
            );
        }
        cx.resolve_promise(r, Arg::Undefined)
    });
    method(reg, rt, OAC, "suspend", |cx, w| {
        need_args(cx, 1, "suspend", OAC)?;
        let _c = this_ctx(cx, w)?;
        let r = cx.ret_new_promise()?;
        cx.resolve_promise(r, Arg::Undefined)
    });
    getter(
        reg,
        rt,
        "OfflineAudioCompletionEvent",
        "renderedBuffer",
        |cx, w| {
            let id = this_obj(cx)?;
            let h = w
                .with_obj(id, |o| match o {
                    Obj::Event(e) => match &e.extra {
                        super::EventExtra::Custom(h) => *h,
                        _ => None,
                    },
                    _ => None,
                })
                .flatten();
            match h {
                Some(h) => cx.ret(Arg::Handle(h)),
                None => cx.ret(Arg::Null),
            }
        },
    );
}

fn settle_with_statechange(w: &Window, r: lb_js::Resolver, obj: u32, changed: bool) {
    w.queue_task(
        TaskSource::DomManipulation,
        Box::new(move |js, w| {
            if changed {
                super::fire_event(js, w, HostRef::new(K_OBJ, obj), "statechange", false, false);
            }
            js.resolve(r, Arg::Undefined);
        }),
    );
}

/// Copies current `AudioBuffer` contents into the buffer sources.
fn refresh_buffer_sources(cx: &mut CallCtx<'_, '_, '_>, w: &Window, c: &Rc<RefCell<Ctx>>) {
    let pairs: Vec<(usize, u32)> = c.borrow().buffers.iter().map(|(a, b)| (*a, *b)).collect();
    for (node, bid) in pairs {
        if let Some(AudioObj::Buffer(b)) = get_a(w, bid) {
            let data = buffer_data(cx, &b);
            let rate = b.borrow().sample_rate;
            if let Kind::BufferSource {
                buffer,
                buffer_rate,
                ..
            } = &mut c.borrow_mut().graph.nodes[node].kind
            {
                *buffer = Some(Rc::new(data));
                *buffer_rate = rate;
            }
        }
    }
}

const FACTORIES: &[(&str, &str)] = &[
    ("createOscillator", "OscillatorNode"),
    ("createGain", "GainNode"),
    ("createDynamicsCompressor", "DynamicsCompressorNode"),
    ("createAnalyser", "AnalyserNode"),
    ("createBiquadFilter", "BiquadFilterNode"),
    ("createBufferSource", "AudioBufferSourceNode"),
    ("createConstantSource", "ConstantSourceNode"),
    ("createStereoPanner", "StereoPannerNode"),
    ("createWaveShaper", "WaveShaperNode"),
    ("createPanner", "PannerNode"),
    ("createConvolver", "ConvolverNode"),
];

fn check_open(w: &Window, c: &Rc<RefCell<Ctx>>, op: &str) -> Result<(), HostError> {
    if c.borrow().state == "closed" && !c.borrow().offline {
        return Err(w.rt.err(
            "InvalidStateError",
            format!(
                "Failed to execute '{op}' on 'BaseAudioContext': AudioContext has been closed."
            ),
        ));
    }
    Ok(())
}

fn register_factories(reg: &mut Registry, rt: &Runtime) {
    const B: &str = "BaseAudioContext";
    for (op, iface) in FACTORIES {
        let (op, iface) = (*op, *iface);
        method(reg, rt, B, op, move |cx, w| {
            let c = this_ctx(cx, w)?;
            check_open(w, &c, op)?;
            let id = create_node(w, &c, iface);
            cx.ret(w.obj_arg(id))
        });
    }
    method(reg, rt, B, "createDelay", |cx, w| {
        let c = this_ctx(cx, w)?;
        let max = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
            cx.arg_f64(0)?
        } else {
            1.0
        };
        if !(max > 0.0 && max < 180.0) {
            return Err(w.rt.err(
                "NotSupportedError",
                format!("Failed to execute 'createDelay' on 'BaseAudioContext': The max delay time provided ({max}) is outside the range (0, 180)."),
            ));
        }
        let id = create_node(w, &c, "DelayNode");
        set_delay_max(w, id, max);
        cx.ret(w.obj_arg(id))
    });
    for (op, iface) in [
        ("createChannelMerger", "ChannelMergerNode"),
        ("createChannelSplitter", "ChannelSplitterNode"),
    ] {
        method(reg, rt, B, op, move |cx, w| {
            let c = this_ctx(cx, w)?;
            let n = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
                cx.arg_f64(0)?
            } else {
                6.0
            };
            if !(1.0..=32.0).contains(&n) {
                return Err(w.rt.err(
                    "IndexSizeError",
                    format!("Failed to execute '{op}' on 'BaseAudioContext': The number of {} provided ({n}) is outside the range [1, 32].", if iface == "ChannelMergerNode" { "inputs" } else { "outputs" }),
                ));
            }
            let id = create_node(w, &c, iface);
            set_channels_n(w, id, n as usize);
            cx.ret(w.obj_arg(id))
        });
    }
    method(reg, rt, B, "createScriptProcessor", |cx, w| {
        let c = this_ctx(cx, w)?;
        let size = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
            cx.arg_u32(0)?
        } else {
            0
        };
        if size != 0 && !(256..=16384).contains(&size) || size & size.wrapping_sub(1) != 0 {
            return Err(w.rt.err(
                "IndexSizeError",
                format!("Failed to execute 'createScriptProcessor' on 'BaseAudioContext': buffer size ({size}) must be 0 or a power of two between 256 and 16384."),
            ));
        }
        let id = create_node(w, &c, "ScriptProcessorNode");
        if let Some(AudioObj::Node { ctx, node, .. }) = get_a(w, id) {
            ctx.borrow_mut().props.insert(
                (node, "bufferSize"),
                Prop::Num(if size == 0 { 1024.0 } else { f64::from(size) }),
            );
        }
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, B, "createIIRFilter", |cx, w| {
        need_args(cx, 2, "createIIRFilter", B)?;
        let c = this_ctx(cx, w)?;
        let ff = arg_floats(cx, 0).unwrap_or_default();
        let fb = arg_floats(cx, 1).unwrap_or_default();
        if ff.is_empty() || ff.len() > 20 || fb.is_empty() || fb.len() > 20 {
            return Err(w.rt.err(
                "NotSupportedError",
                "Failed to execute 'createIIRFilter' on 'BaseAudioContext': coefficients must have 1 to 20 values.",
            ));
        }
        if fb[0] == 0.0 {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'createIIRFilter' on 'BaseAudioContext': First feedback coefficient cannot be zero.",
            ));
        }
        let id = create_node(w, &c, "IIRFilterNode");
        if let Some(AudioObj::Node { ctx, node, .. }) = get_a(w, id)
            && let Kind::Iir { ff: a, fb: b, .. } = &mut ctx.borrow_mut().graph.nodes[node].kind
        {
            *a = ff.iter().map(|v| f64::from(*v)).collect();
            *b = fb.iter().map(|v| f64::from(*v)).collect();
        }
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, B, "createBuffer", |cx, w| {
        need_args(cx, 3, "createBuffer", B)?;
        let ch = cx.arg_f64(0)?.trunc();
        let len = cx.arg_f64(1)?.trunc();
        let sr = cx.arg_f64(2)?;
        if let Some(e) = buffer_error(w, "createBuffer", ch, len, sr) {
            return Err(e);
        }
        let id = new_buffer(
            w,
            sr as f32,
            len as usize,
            vec![vec![0.0; len as usize]; ch as usize],
        );
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, B, "createPeriodicWave", |cx, w| {
        need_args(cx, 2, "createPeriodicWave", B)?;
        let c = this_ctx(cx, w)?;
        let real = arg_floats(cx, 0).unwrap_or_default();
        let imag = arg_floats(cx, 1).unwrap_or_default();
        if real.len() != imag.len() {
            return Err(w.rt.err(
                "IndexSizeError",
                format!("Failed to execute 'createPeriodicWave' on 'BaseAudioContext': length of real array ({}) and length of imaginary array ({}) must match.", real.len(), imag.len()),
            ));
        }
        if real.len() < 2 {
            return Err(w.rt.err(
                "IndexSizeError",
                format!("Failed to execute 'createPeriodicWave' on 'BaseAudioContext': length of real array ({}) must be at least 2.", real.len()),
            ));
        }
        let d = Dict::arg(cx, 2);
        let disable = d.bool(cx, "disableNormalization")?;
        d.release(cx);
        let sr = c.borrow().graph.sample_rate;
        let id = new_a(
            w,
            AudioObj::Wave(Rc::new(PeriodicWave::new(sr, &real, &imag, disable))),
        );
        cx.ret(w.obj_arg(id))
    });
    method(reg, rt, B, "decodeAudioData", |cx, w| {
        need_args(cx, 1, "decodeAudioData", B)?;
        let err_cb =
            (cx.len() > 2 && cx.arg_kind(2) == ValueKind::Function).then(|| cx.arg_handle(2));
        let r = cx.ret_new_promise()?;
        let e = w.rt.err("EncodingError", "Unable to decode audio data");
        if let Some(f) = err_cb
            && let HostError::Throw(a) = w.rt.err("EncodingError", "Unable to decode audio data")
        {
            w.queue_task(
                TaskSource::DomManipulation,
                Box::new(move |js, _w| {
                    if let Some(h) = js.make(a) {
                        let _ = js.call(f, Arg::Undefined, &[Arg::Handle(h)]);
                        js.release(h);
                    }
                    js.release(f);
                }),
            );
        }
        cx.reject_promise(r, e)
    });
}

fn set_delay_max(w: &Window, id: u32, max: f64) {
    if let Some(AudioObj::Node { ctx, node, .. }) = get_a(w, id) {
        let mut c = ctx.borrow_mut();
        if let Kind::Delay { max: m, .. } = &mut c.graph.nodes[node].kind {
            *m = max;
        }
        c.graph.nodes[node].params[0].max = max as f32;
    }
}

fn set_channels_n(w: &Window, id: u32, n: usize) {
    if let Some(AudioObj::Node { ctx, node, .. }) = get_a(w, id) {
        let mut c = ctx.borrow_mut();
        let nd = &mut c.graph.nodes[node];
        match &mut nd.kind {
            Kind::Merger(k) => *k = n,
            Kind::Splitter(k) => {
                *k = n;
                nd.channel_count = n;
            }
            _ => {}
        }
    }
}

fn node_counts(c: &Ctx, node: usize, iface: &str) -> (u32, u32) {
    if node == 0 {
        return (1, 0);
    }
    let sp = spec(iface);
    let (mut i, mut o) = sp.map_or((1, 1), |s| (s.inputs, s.outputs));
    match &c.graph.nodes[node].kind {
        Kind::Merger(n) => i = *n as u32,
        Kind::Splitter(n) => o = *n as u32,
        _ => {}
    }
    (i, o)
}

fn register_node_base(reg: &mut Registry, rt: &Runtime) {
    const N: &str = "AudioNode";
    getter(reg, rt, N, "context", |cx, w| {
        let (c, _) = this_node(cx, w)?;
        let id = c.borrow().obj;
        cx.ret(w.obj_arg(id))
    });
    getter(reg, rt, N, "numberOfInputs", |cx, w| {
        let a = this_a(cx, w)?;
        let AudioObj::Node { ctx, node, iface } = a else {
            return Err(illegal());
        };
        let n = node_counts(&ctx.borrow(), node, iface).0;
        cx.ret(n)
    });
    getter(reg, rt, N, "numberOfOutputs", |cx, w| {
        let a = this_a(cx, w)?;
        let AudioObj::Node { ctx, node, iface } = a else {
            return Err(illegal());
        };
        let n = node_counts(&ctx.borrow(), node, iface).1;
        cx.ret(n)
    });
    getter(reg, rt, N, "channelCount", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = c.borrow().graph.nodes[n].channel_count;
        cx.ret(v as u32)
    });
    setter(reg, rt, N, "channelCount", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = cx.arg_u32(0)?;
        let max = if n == 0 { c.borrow().max_channels } else { 32 };
        if v == 0 || v > max {
            return Err(w.rt.err(
                "NotSupportedError",
                format!("Failed to set the 'channelCount' property on 'AudioNode': The channel count provided ({v}) is outside the range [1, {max}]."),
            ));
        }
        c.borrow_mut().graph.nodes[n].channel_count = v as usize;
        Ok(())
    });
    getter(reg, rt, N, "channelCountMode", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = c.borrow().graph.nodes[n].mode.as_str();
        cx.ret(v)
    });
    setter(reg, rt, N, "channelCountMode", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let s = cx.arg_string(0)?;
        if let Some(m) = CountMode::parse(&s) {
            c.borrow_mut().graph.nodes[n].mode = m;
        }
        Ok(())
    });
    getter(reg, rt, N, "channelInterpretation", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let d = c.borrow().graph.nodes[n].discrete;
        cx.ret(if d { "discrete" } else { "speakers" })
    });
    setter(reg, rt, N, "channelInterpretation", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let s = cx.arg_string(0)?;
        match s.as_str() {
            "discrete" => c.borrow_mut().graph.nodes[n].discrete = true,
            "speakers" => c.borrow_mut().graph.nodes[n].discrete = false,
            _ => {}
        }
        Ok(())
    });
    method(reg, rt, N, "connect", |cx, w| {
        need_args(cx, 1, "connect", N)?;
        let (c, n) = this_node(cx, w)?;
        let output = if cx.len() > 1 {
            cx.arg_u32(1)? as usize
        } else {
            0
        };
        let input = if cx.len() > 2 {
            cx.arg_u32(2)? as usize
        } else {
            0
        };
        let Some((did, dest)) = arg_a(cx, w, 0) else {
            return Err(type_err("connect", N, "Overload resolution failed."));
        };
        let same = |o: &Rc<RefCell<Ctx>>| Rc::ptr_eq(o, &c);
        match dest {
            AudioObj::Node { ctx, node, .. } => {
                if !same(&ctx) {
                    return Err(w.rt.err(
                        "InvalidAccessError",
                        "Failed to execute 'connect' on 'AudioNode': cannot connect to an AudioNode belonging to a different audio context.",
                    ));
                }
                let mut cc = c.borrow_mut();
                let outs = {
                    let iface = spec_iface_of(&cc, n);
                    node_counts(&cc, n, iface).1
                };
                if output as u32 >= outs.max(1) {
                    drop(cc);
                    return Err(w.rt.err(
                        "IndexSizeError",
                        format!("Failed to execute 'connect' on 'AudioNode': output index ({output}) exceeds number of outputs ({outs})."),
                    ));
                }
                let conn = (n, output, input);
                if !cc.graph.nodes[node].inputs.contains(&conn) {
                    cc.graph.nodes[node].inputs.push(conn);
                }
                drop(cc);
                cx.ret(w.obj_arg(did))
            }
            AudioObj::Param { ctx, node, param } => {
                if !same(&ctx) {
                    return Err(w.rt.err(
                        "InvalidAccessError",
                        "Failed to execute 'connect' on 'AudioNode': cannot connect to an AudioParam belonging to a different audio context.",
                    ));
                }
                let mut cc = c.borrow_mut();
                let p = &mut cc.graph.nodes[node].params[param];
                if !p.inputs.contains(&n) {
                    p.inputs.push(n);
                }
                Ok(())
            }
            _ => Err(type_err("connect", N, "Overload resolution failed.")),
        }
    });
    method(reg, rt, N, "disconnect", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let target = if !cx.is_empty() && cx.arg_kind(0) == ValueKind::Object {
            arg_a(cx, w, 0).map(|(_, a)| a)
        } else {
            None
        };
        let mut cc = c.borrow_mut();
        match target {
            Some(AudioObj::Node { node, .. }) => {
                cc.graph.nodes[node].inputs.retain(|x| x.0 != n);
            }
            Some(AudioObj::Param { node, param, .. }) => {
                cc.graph.nodes[node].params[param]
                    .inputs
                    .retain(|x| *x != n);
            }
            _ => {
                for nd in &mut cc.graph.nodes {
                    nd.inputs.retain(|x| x.0 != n);
                    for p in &mut nd.params {
                        p.inputs.retain(|x| *x != n);
                    }
                }
            }
        }
        Ok(())
    });
    getter(
        reg,
        rt,
        "AudioDestinationNode",
        "maxChannelCount",
        |cx, w| {
            let (c, _) = this_node(cx, w)?;
            let v = c.borrow().max_channels;
            cx.ret(v)
        },
    );
    // Scheduled sources.
    const S: &str = "AudioScheduledSourceNode";
    method(reg, rt, S, "start", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let when = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
            cx.arg_f64(0)?
        } else {
            0.0
        };
        if when < 0.0 || !when.is_finite() {
            return Err(HostError::range_error(format!(
                "Failed to execute 'start' on 'AudioScheduledSourceNode': The start time provided ({when}) is less than the minimum bound (0)."
            )));
        }
        let offset = if cx.len() > 1 && cx.arg_kind(1) != ValueKind::Undefined {
            cx.arg_f64(1)?.max(0.0)
        } else {
            0.0
        };
        let mut cc = c.borrow_mut();
        let now = cc.graph.time();
        let sched = match &mut cc.graph.nodes[n].kind {
            Kind::Oscillator { sched, .. } | Kind::Constant { sched } => sched,
            Kind::BufferSource {
                sched, offset: o, ..
            } => {
                *o = offset;
                sched
            }
            _ => return Err(illegal()),
        };
        if sched.start.is_some() {
            drop(cc);
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'start' on 'AudioScheduledSourceNode': cannot call start more than once.",
            ));
        }
        sched.start = Some(when.max(now));
        Ok(())
    });
    method(reg, rt, S, "stop", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let when = if !cx.is_empty() && cx.arg_kind(0) != ValueKind::Undefined {
            cx.arg_f64(0)?
        } else {
            0.0
        };
        if when < 0.0 || !when.is_finite() {
            return Err(HostError::range_error(format!(
                "Failed to execute 'stop' on 'AudioScheduledSourceNode': The stop time provided ({when}) is less than the minimum bound (0)."
            )));
        }
        let mut cc = c.borrow_mut();
        let now = cc.graph.time();
        let sched = match &mut cc.graph.nodes[n].kind {
            Kind::Oscillator { sched, .. }
            | Kind::Constant { sched }
            | Kind::BufferSource { sched, .. } => sched,
            _ => return Err(illegal()),
        };
        if sched.start.is_none() {
            drop(cc);
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'stop' on 'AudioScheduledSourceNode': cannot call stop without calling start first.",
            ));
        }
        sched.stop = Some(when.max(now));
        Ok(())
    });
}

fn spec_iface_of(c: &Ctx, n: usize) -> &'static str {
    match &c.graph.nodes[n].kind {
        Kind::Splitter(_) => "ChannelSplitterNode",
        Kind::Merger(_) => "ChannelMergerNode",
        Kind::Destination => "AudioDestinationNode",
        _ => "GainNode",
    }
}

/// Param attributes per node interface.
const PARAM_ATTRS: &[(&str, &str, usize)] = &[
    ("OscillatorNode", "frequency", 0),
    ("OscillatorNode", "detune", 1),
    ("GainNode", "gain", 0),
    ("DynamicsCompressorNode", "threshold", 0),
    ("DynamicsCompressorNode", "knee", 1),
    ("DynamicsCompressorNode", "ratio", 2),
    ("DynamicsCompressorNode", "attack", 3),
    ("DynamicsCompressorNode", "release", 4),
    ("BiquadFilterNode", "frequency", 0),
    ("BiquadFilterNode", "detune", 1),
    ("BiquadFilterNode", "Q", 2),
    ("BiquadFilterNode", "gain", 3),
    ("AudioBufferSourceNode", "playbackRate", 0),
    ("AudioBufferSourceNode", "detune", 1),
    ("ConstantSourceNode", "offset", 0),
    ("DelayNode", "delayTime", 0),
    ("StereoPannerNode", "pan", 0),
    ("PannerNode", "positionX", 0),
    ("PannerNode", "positionY", 1),
    ("PannerNode", "positionZ", 2),
    ("PannerNode", "orientationX", 3),
    ("PannerNode", "orientationY", 4),
    ("PannerNode", "orientationZ", 5),
];

fn param_obj(w: &Window, ctx: &Rc<RefCell<Ctx>>, node: usize, param: usize) -> u32 {
    let cobj = ctx.borrow().obj;
    let c2 = ctx.clone();
    w.cached_obj(
        (
            cobj,
            (node * 64 + param) as u32,
            "AudioParam",
            String::new(),
        ),
        move || {
            Obj::Audio(Box::new(AudioObj::Param {
                ctx: c2,
                node,
                param,
            }))
        },
    )
}

fn this_param(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
) -> Result<(Rc<RefCell<Ctx>>, usize, usize), HostError> {
    match this_a(cx, w)? {
        AudioObj::Param { ctx, node, param } => Ok((ctx, node, param)),
        _ => Err(illegal()),
    }
}

fn register_params(reg: &mut Registry, rt: &Runtime) {
    for (iface, attr, p) in PARAM_ATTRS {
        let p = *p;
        getter(reg, rt, iface, attr, move |cx, w| {
            let (c, n) = this_node(cx, w)?;
            let id = param_obj(w, &c, n, p);
            cx.ret(w.obj_arg(id))
        });
    }
    for (k, (attr, ..)) in LISTENER_PARAMS.iter().enumerate() {
        getter(reg, rt, "AudioListener", attr, move |cx, w| {
            let AudioObj::Listener { ctx } = this_a(cx, w)? else {
                return Err(illegal());
            };
            let Some(n) = ctx.borrow().listener_node else {
                return Err(illegal());
            };
            let id = param_obj(w, &ctx, n, k);
            cx.ret(w.obj_arg(id))
        });
    }
    for op in ["setPosition", "setOrientation"] {
        method(reg, rt, "AudioListener", op, |_cx, _w| Ok(()));
        method(reg, rt, "PannerNode", op, |_cx, _w| Ok(()));
    }
    const P: &str = "AudioParam";
    getter(reg, rt, P, "value", |cx, w| {
        let (c, n, p) = this_param(cx, w)?;
        sync(w, &c);
        let cc = c.borrow();
        let prm = &cc.graph.nodes[n].params[p];
        let v = if prm.events.is_empty() {
            prm.value
        } else {
            prm.at(cc.graph.time())
        };
        cx.ret(f64::from(v))
    });
    setter(reg, rt, P, "value", |cx, w| {
        let (c, n, p) = this_param(cx, w)?;
        let v = cx.arg_f64(0)?;
        if !v.is_finite() {
            return Err(HostError::type_error(
                "Failed to set the 'value' property on 'AudioParam': The provided float value is non-finite.",
            ));
        }
        let mut cc = c.borrow_mut();
        let t = cc.graph.time();
        let prm = &mut cc.graph.nodes[n].params[p];
        prm.value = v as f32;
        if !prm.events.is_empty() {
            prm.insert(Event::Set { t, v: v as f32 });
        }
        Ok(())
    });
    for (attr, k) in [("defaultValue", 0), ("minValue", 1), ("maxValue", 2)] {
        getter(reg, rt, P, attr, move |cx, w| {
            let (c, n, p) = this_param(cx, w)?;
            let cc = c.borrow();
            let prm = &cc.graph.nodes[n].params[p];
            let v = [prm.default, prm.min, prm.max][k];
            cx.ret(f64::from(v))
        });
    }
    getter(reg, rt, P, "automationRate", |cx, w| {
        let (c, n, p) = this_param(cx, w)?;
        let a = c.borrow().graph.nodes[n].params[p].a_rate;
        cx.ret(if a { "a-rate" } else { "k-rate" })
    });
    setter(reg, rt, P, "automationRate", |cx, w| {
        let (c, n, p) = this_param(cx, w)?;
        let s = cx.arg_string(0)?;
        match s.as_str() {
            "a-rate" => c.borrow_mut().graph.nodes[n].params[p].a_rate = true,
            "k-rate" => c.borrow_mut().graph.nodes[n].params[p].a_rate = false,
            _ => {}
        }
        Ok(())
    });
    type Mk = fn(f64, f32, f64) -> Event;
    let simple: [(&str, Mk); 3] = [
        ("setValueAtTime", |t, v, _| Event::Set { t, v }),
        ("linearRampToValueAtTime", |t, v, _| Event::Linear { t, v }),
        ("exponentialRampToValueAtTime", |t, v, _| Event::Exp {
            t,
            v,
        }),
    ];
    for (op, mk) in simple {
        method(reg, rt, P, op, move |cx, w| {
            need_args(cx, 2, op, P)?;
            let (c, n, p) = this_param(cx, w)?;
            let v = cx.arg_f64(0)?;
            let t = cx.arg_f64(1)?;
            if !v.is_finite() || !t.is_finite() {
                return Err(HostError::type_error(format!(
                    "Failed to execute '{op}' on 'AudioParam': The provided float value is non-finite."
                )));
            }
            if t < 0.0 {
                return Err(HostError::range_error(format!(
                    "Failed to execute '{op}' on 'AudioParam': Time must be a finite non-negative number: {t}"
                )));
            }
            if op == "exponentialRampToValueAtTime" && v == 0.0 {
                return Err(HostError::range_error(format!(
                    "Failed to execute '{op}' on 'AudioParam': The float target value provided (0) should not be in the range (-1.40130e-45, 1.40130e-45)."
                )));
            }
            c.borrow_mut().graph.nodes[n].params[p].insert(mk(t, v as f32, 0.0));
            ret_this(cx, w)
        });
    }
    method(reg, rt, P, "setTargetAtTime", |cx, w| {
        need_args(cx, 3, "setTargetAtTime", P)?;
        let (c, n, p) = this_param(cx, w)?;
        let v = cx.arg_f64(0)?;
        let t = cx.arg_f64(1)?;
        let tau = cx.arg_f64(2)?;
        if t < 0.0 || tau < 0.0 {
            return Err(HostError::range_error(format!(
                "Failed to execute 'setTargetAtTime' on 'AudioParam': Time must be a finite non-negative number: {t}"
            )));
        }
        c.borrow_mut().graph.nodes[n].params[p].insert(Event::Target {
            t,
            v: v as f32,
            tau,
        });
        ret_this(cx, w)
    });
    method(reg, rt, P, "setValueCurveAtTime", |cx, w| {
        need_args(cx, 3, "setValueCurveAtTime", P)?;
        let (c, n, p) = this_param(cx, w)?;
        let curve = arg_floats(cx, 0).unwrap_or_default();
        let t = cx.arg_f64(1)?;
        let dur = cx.arg_f64(2)?;
        if curve.len() < 2 {
            return Err(w.rt.err(
                "InvalidStateError",
                format!("Failed to execute 'setValueCurveAtTime' on 'AudioParam': Curve length must be at least 2: {}", curve.len()),
            ));
        }
        if t < 0.0 || dur <= 0.0 {
            return Err(HostError::range_error(format!(
                "Failed to execute 'setValueCurveAtTime' on 'AudioParam': Time must be a finite non-negative number: {t}"
            )));
        }
        c.borrow_mut().graph.nodes[n].params[p].insert(Event::Curve { t, dur, curve });
        ret_this(cx, w)
    });
    for op in ["cancelScheduledValues", "cancelAndHoldAtTime"] {
        method(reg, rt, P, op, move |cx, w| {
            need_args(cx, 1, op, P)?;
            let (c, n, p) = this_param(cx, w)?;
            let t = cx.arg_f64(0)?;
            if t < 0.0 {
                return Err(HostError::range_error(format!(
                    "Failed to execute '{op}' on 'AudioParam': Time must be a finite non-negative number: {t}"
                )));
            }
            let mut cc = c.borrow_mut();
            let prm = &mut cc.graph.nodes[n].params[p];
            if op == "cancelAndHoldAtTime" {
                let v = prm.at(t);
                prm.cancel(t);
                prm.insert(Event::Set { t, v });
            } else {
                prm.cancel(t);
            }
            drop(cc);
            ret_this(cx, w)
        });
    }
}

/// Node constructors (`new GainNode(ctx, options)`).
fn register_constructors(reg: &mut Registry, rt: &Runtime) {
    for iface in [
        "OscillatorNode",
        "GainNode",
        "DynamicsCompressorNode",
        "AnalyserNode",
        "BiquadFilterNode",
        "AudioBufferSourceNode",
        "ConstantSourceNode",
        "DelayNode",
        "StereoPannerNode",
        "WaveShaperNode",
        "ChannelMergerNode",
        "ChannelSplitterNode",
        "PannerNode",
        "ConvolverNode",
    ] {
        ctor(reg, rt, iface, move |cx, w| {
            let Some((_, AudioObj::Context { ctx, .. })) = arg_a(cx, w, 0) else {
                return Err(HostError::type_error(format!(
                    "Failed to construct '{iface}': parameter 1 is not of type 'BaseAudioContext'."
                )));
            };
            let id = create_node(w, &ctx, iface);
            cx.init_this(HostRef::new(K_OBJ, id))?;
            let d = Dict::arg(cx, 1);
            let (node, sr) = match get_a(w, id) {
                Some(AudioObj::Node { node, .. }) => (node, ctx.borrow().graph.sample_rate),
                _ => return Ok(()),
            };
            let _ = sr;
            for (i2, attr, p) in PARAM_ATTRS {
                if *i2 == iface
                    && let Some(v) = d.number(cx, attr)?
                {
                    ctx.borrow_mut().graph.nodes[node].params[*p].value = v as f32;
                }
            }
            if let Some(v) = d.number(cx, "channelCount")? {
                ctx.borrow_mut().graph.nodes[node].channel_count = v as usize;
            }
            if let Some(s) = d.string(cx, "channelCountMode")?
                && let Some(m) = CountMode::parse(&s)
            {
                ctx.borrow_mut().graph.nodes[node].mode = m;
            }
            match iface {
                "OscillatorNode" => {
                    if let Some(s) = d.string(cx, "type")?
                        && let Some(sh) = Shape::parse(&s)
                        && sh != Shape::Custom
                        && let Kind::Oscillator { shape, .. } =
                            &mut ctx.borrow_mut().graph.nodes[node].kind
                    {
                        *shape = sh;
                    }
                }
                "BiquadFilterNode" => {
                    if let Some(s) = d.string(cx, "type")?
                        && let Some(t) = BiquadType::parse(&s)
                        && let Kind::Biquad { ty, .. } =
                            &mut ctx.borrow_mut().graph.nodes[node].kind
                    {
                        *ty = t;
                    }
                }
                "DelayNode" => {
                    if let Some(m) = d.number(cx, "maxDelayTime")? {
                        set_delay_max(w, id, m);
                    }
                }
                "ChannelMergerNode" => {
                    if let Some(m) = d.number(cx, "numberOfInputs")? {
                        set_channels_n(w, id, m as usize);
                    }
                }
                "ChannelSplitterNode" => {
                    if let Some(m) = d.number(cx, "numberOfOutputs")? {
                        set_channels_n(w, id, m as usize);
                    }
                }
                "AnalyserNode" => {
                    if let Some(v) = d.number(cx, "fftSize")?
                        && let Kind::Analyser(a) = &mut ctx.borrow_mut().graph.nodes[node].kind
                    {
                        a.set_fft_size(v as usize);
                    }
                }
                _ => {}
            }
            d.release(cx);
            Ok(())
        });
    }
}

fn this_analyser<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&mut Analyser, u64) -> R,
) -> Result<R, HostError> {
    let (c, n) = this_node(cx, w)?;
    sync(w, &c);
    let mut cc = c.borrow_mut();
    let frame = cc.graph.frame;
    match &mut cc.graph.nodes[n].kind {
        Kind::Analyser(a) => Ok(f(a, frame)),
        _ => Err(illegal()),
    }
}

fn register_nodes(reg: &mut Registry, rt: &Runtime) {
    register_constructors(reg, rt);
    // Oscillator
    const O: &str = "OscillatorNode";
    getter(reg, rt, O, "type", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let s = match &c.borrow().graph.nodes[n].kind {
            Kind::Oscillator { shape, .. } => shape.as_str(),
            _ => return Err(illegal()),
        };
        cx.ret(s)
    });
    setter(reg, rt, O, "type", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let s = cx.arg_string(0)?;
        let Some(sh) = Shape::parse(&s) else {
            return Ok(());
        };
        if sh == Shape::Custom {
            return Err(w.rt.err(
                "InvalidStateError",
                "Failed to set the 'type' property on 'OscillatorNode': 'type' cannot be set directly to 'custom'.  Use setPeriodicWave() to create a custom Oscillator type.",
            ));
        }
        if let Kind::Oscillator { shape, wave, .. } = &mut c.borrow_mut().graph.nodes[n].kind {
            if *shape != sh {
                *wave = None;
            }
            *shape = sh;
        }
        Ok(())
    });
    method(reg, rt, O, "setPeriodicWave", |cx, w| {
        need_args(cx, 1, "setPeriodicWave", O)?;
        let (c, n) = this_node(cx, w)?;
        let Some((_, AudioObj::Wave(pw))) = arg_a(cx, w, 0) else {
            return Err(type_err(
                "setPeriodicWave",
                O,
                "parameter 1 is not of type 'PeriodicWave'.",
            ));
        };
        if let Kind::Oscillator { shape, custom, .. } = &mut c.borrow_mut().graph.nodes[n].kind {
            *shape = Shape::Custom;
            *custom = Some(pw);
        }
        Ok(())
    });
    ctor(reg, rt, "PeriodicWave", |cx, w| {
        let Some((_, AudioObj::Context { ctx, .. })) = arg_a(cx, w, 0) else {
            return Err(HostError::type_error(
                "Failed to construct 'PeriodicWave': parameter 1 is not of type 'BaseAudioContext'.",
            ));
        };
        let d = Dict::arg(cx, 1);
        let real = match d.get(cx, "real")? {
            Some(h) => {
                let b = cx.handle_bytes(h).map(|b| bytes_to_f32s(&b));
                cx.release(h);
                b
            }
            None => None,
        };
        let imag = match d.get(cx, "imag")? {
            Some(h) => {
                let b = cx.handle_bytes(h).map(|b| bytes_to_f32s(&b));
                cx.release(h);
                b
            }
            None => None,
        };
        let disable = d.bool(cx, "disableNormalization")?;
        d.release(cx);
        let n = real.as_ref().or(imag.as_ref()).map_or(2, Vec::len);
        let real = real.unwrap_or_else(|| vec![0.0; n]);
        let mut imag = imag.unwrap_or_else(|| vec![0.0; n]);
        if imag.len() == 2 && imag.iter().all(|v| *v == 0.0) && real.iter().all(|v| *v == 0.0) {
            imag[1] = 1.0;
        }
        let sr = ctx.borrow().graph.sample_rate;
        let id = new_a(
            w,
            AudioObj::Wave(Rc::new(PeriodicWave::new(sr, &real, &imag, disable))),
        );
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    // Compressor
    getter(reg, rt, "DynamicsCompressorNode", "reduction", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let r = match &c.borrow().graph.nodes[n].kind {
            Kind::Compressor { reduction, .. } => *reduction,
            _ => return Err(illegal()),
        };
        cx.ret(f64::from(r))
    });
    // Analyser
    const A: &str = "AnalyserNode";
    getter(reg, rt, A, "fftSize", |cx, w| {
        let v = this_analyser(cx, w, |a, _| a.fft_size)?;
        cx.ret(v as u32)
    });
    setter(reg, rt, A, "fftSize", |cx, w| {
        let v = cx.arg_u32(0)? as usize;
        if !(32..=dsp::MAX_FFT).contains(&v) || !v.is_power_of_two() {
            let msg = if v.is_power_of_two() || v == 0 {
                format!(
                    "Failed to set the 'fftSize' property on 'AnalyserNode': The value provided ({v}) is outside the range [32, 32768]."
                )
            } else {
                format!(
                    "Failed to set the 'fftSize' property on 'AnalyserNode': The value provided ({v}) is not a power of two."
                )
            };
            return Err(w.rt.err("IndexSizeError", msg));
        }
        this_analyser(cx, w, |a, _| a.set_fft_size(v))
    });
    getter(reg, rt, A, "frequencyBinCount", |cx, w| {
        let v = this_analyser(cx, w, |a, _| a.fft_size / 2)?;
        cx.ret(v as u32)
    });
    for (attr, k) in [
        ("minDecibels", 0),
        ("maxDecibels", 1),
        ("smoothingTimeConstant", 2),
    ] {
        getter(reg, rt, A, attr, move |cx, w| {
            let v = this_analyser(cx, w, |a, _| [a.min_db, a.max_db, a.smoothing][k])?;
            cx.ret(v)
        });
        setter(reg, rt, A, attr, move |cx, w| {
            let v = cx.arg_f64(0)?;
            let (min, max) = this_analyser(cx, w, |a, _| (a.min_db, a.max_db))?;
            let bad = match k {
                0 => (v >= max).then(|| format!("The minDecibels provided ({v}) is greater than or equal to the maximum bound ({max}).")),
                1 => (v <= min).then(|| format!("The maxDecibels provided ({v}) is less than or equal to the minimum bound ({min}).")),
                _ => (!(0.0..=1.0).contains(&v)).then(|| format!("The smoothing value provided ({v}) is outside the range [0, 1].")),
            };
            if let Some(m) = bad {
                return Err(w.rt.err(
                    "IndexSizeError",
                    format!("Failed to set the '{attr}' property on 'AnalyserNode': {m}"),
                ));
            }
            this_analyser(cx, w, |a, _| match k {
                0 => a.min_db = v,
                1 => a.max_db = v,
                _ => a.smoothing = v,
            })
        });
    }
    let ops: [(&str, ViewKind, u8); 4] = [
        ("getFloatFrequencyData", ViewKind::Float32, 0),
        ("getByteFrequencyData", ViewKind::Uint8, 1),
        ("getFloatTimeDomainData", ViewKind::Float32, 2),
        ("getByteTimeDomainData", ViewKind::Uint8, 3),
    ];
    for (op, kind, k) in ops {
        method(reg, rt, A, op, move |cx, w| {
            need_args(cx, 1, op, A)?;
            if cx.arg_view_kind(0) != Some(kind) {
                let t = if kind == ViewKind::Float32 {
                    "Float32Array"
                } else {
                    "Uint8Array"
                };
                return Err(type_err(
                    op,
                    A,
                    &format!("parameter 1 is not of type '{t}'."),
                ));
            }
            let s = seed(w);
            let bytes = this_analyser(cx, w, |a, frame| match k {
                0 => {
                    let mut v = a.float_frequency(frame);
                    if let Some(s) = s {
                        dsp::apply_noise(s, 0x4146, &mut v);
                    }
                    f32s_to_bytes(&v)
                }
                1 => a.byte_frequency(frame),
                2 => {
                    let mut v = a.time_domain();
                    if let Some(s) = s {
                        dsp::apply_noise(s, 0x4154, &mut v);
                    }
                    f32s_to_bytes(&v)
                }
                _ => dsp::byte_time_domain(&a.time_domain()),
            })?;
            cx.write_arg_bytes(0, 0, &bytes);
            Ok(())
        });
    }
    // Biquad
    const BQ: &str = "BiquadFilterNode";
    getter(reg, rt, BQ, "type", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let s = match &c.borrow().graph.nodes[n].kind {
            Kind::Biquad { ty, .. } => ty.as_str(),
            _ => return Err(illegal()),
        };
        cx.ret(s)
    });
    setter(reg, rt, BQ, "type", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let s = cx.arg_string(0)?;
        if let Some(t) = BiquadType::parse(&s)
            && let Kind::Biquad { ty, .. } = &mut c.borrow_mut().graph.nodes[n].kind
        {
            *ty = t;
        }
        Ok(())
    });
    method(reg, rt, BQ, "getFrequencyResponse", |cx, w| {
        need_args(cx, 3, "getFrequencyResponse", BQ)?;
        let (c, n) = this_node(cx, w)?;
        for i in 0..3 {
            if cx.arg_view_kind(i) != Some(ViewKind::Float32) {
                return Err(type_err(
                    "getFrequencyResponse",
                    BQ,
                    &format!("parameter {} is not of type 'Float32Array'.", i + 1),
                ));
            }
        }
        let freqs = arg_floats(cx, 0).unwrap_or_default();
        let cc = c.borrow();
        let sr = f64::from(cc.graph.sample_rate);
        let nd = &cc.graph.nodes[n];
        let Kind::Biquad { ty, .. } = &nd.kind else {
            return Err(illegal());
        };
        let t = cc.graph.time();
        let pv = |k: usize| f64::from(nd.params[k].at(t));
        let co = dsp::biquad_coefficients(*ty, sr, pv(0), pv(2), pv(3), pv(1));
        drop(cc);
        let (mut mag, mut ph) = (Vec::new(), Vec::new());
        for f in &freqs {
            let nf = f64::from(*f) / (sr / 2.0);
            if (0.0..=1.0).contains(&nf) {
                let (m, p) = dsp::response(&[co[0], co[1], co[2]], &[1.0, co[3], co[4]], nf);
                mag.push(m);
                ph.push(p);
            } else {
                mag.push(f32::NAN);
                ph.push(f32::NAN);
            }
        }
        cx.write_arg_bytes(1, 0, &f32s_to_bytes(&mag));
        cx.write_arg_bytes(2, 0, &f32s_to_bytes(&ph));
        Ok(())
    });
    method(reg, rt, "IIRFilterNode", "getFrequencyResponse", |cx, w| {
        need_args(cx, 3, "getFrequencyResponse", "IIRFilterNode")?;
        let (c, n) = this_node(cx, w)?;
        let freqs = arg_floats(cx, 0).unwrap_or_default();
        let cc = c.borrow();
        let sr = f64::from(cc.graph.sample_rate);
        let Kind::Iir { ff, fb, .. } = &cc.graph.nodes[n].kind else {
            return Err(illegal());
        };
        let (ff, fb) = (ff.clone(), fb.clone());
        drop(cc);
        let (mut mag, mut ph) = (Vec::new(), Vec::new());
        for f in &freqs {
            let nf = f64::from(*f) / (sr / 2.0);
            let (m, p) = if (0.0..=1.0).contains(&nf) {
                dsp::response(&ff, &fb, nf)
            } else {
                (f32::NAN, f32::NAN)
            };
            mag.push(m);
            ph.push(p);
        }
        cx.write_arg_bytes(1, 0, &f32s_to_bytes(&mag));
        cx.write_arg_bytes(2, 0, &f32s_to_bytes(&ph));
        Ok(())
    });
    // Buffer source
    const BS: &str = "AudioBufferSourceNode";
    getter(reg, rt, BS, "buffer", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let id = c.borrow().buffers.get(&n).copied();
        match id {
            Some(id) => cx.ret(w.obj_arg(id)),
            None => cx.ret(Arg::Null),
        }
    });
    setter(reg, rt, BS, "buffer", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        if cx.arg_kind(0) == ValueKind::Null {
            c.borrow_mut().buffers.remove(&n);
            return Ok(());
        }
        let Some((bid, AudioObj::Buffer(b))) = arg_a(cx, w, 0) else {
            return Err(HostError::type_error(
                "Failed to set the 'buffer' property on 'AudioBufferSourceNode': The provided value is not of type 'AudioBuffer'.",
            ));
        };
        let data = buffer_data(cx, &b);
        let rate = b.borrow().sample_rate;
        let mut cc = c.borrow_mut();
        cc.buffers.insert(n, bid);
        if let Kind::BufferSource {
            buffer,
            buffer_rate,
            ..
        } = &mut cc.graph.nodes[n].kind
        {
            *buffer = Some(Rc::new(data));
            *buffer_rate = rate;
        }
        Ok(())
    });
    getter(reg, rt, BS, "loop", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = matches!(
            c.borrow().graph.nodes[n].kind,
            Kind::BufferSource { looping: true, .. }
        );
        cx.ret(v)
    });
    setter(reg, rt, BS, "loop", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = cx.arg_bool(0);
        if let Kind::BufferSource { looping, .. } = &mut c.borrow_mut().graph.nodes[n].kind {
            *looping = v;
        }
        Ok(())
    });
    for (attr, k) in [("loopStart", 0), ("loopEnd", 1)] {
        getter(reg, rt, BS, attr, move |cx, w| {
            let (c, n) = this_node(cx, w)?;
            let v = match &c.borrow().graph.nodes[n].kind {
                Kind::BufferSource {
                    loop_start,
                    loop_end,
                    ..
                } => [*loop_start, *loop_end][k],
                _ => 0.0,
            };
            cx.ret(v)
        });
        setter(reg, rt, BS, attr, move |cx, w| {
            let (c, n) = this_node(cx, w)?;
            let v = cx.arg_f64(0)?;
            if let Kind::BufferSource {
                loop_start,
                loop_end,
                ..
            } = &mut c.borrow_mut().graph.nodes[n].kind
            {
                if k == 0 {
                    *loop_start = v;
                } else {
                    *loop_end = v;
                }
            }
            Ok(())
        });
    }
    // Wave shaper
    const WS: &str = "WaveShaperNode";
    getter(reg, rt, WS, "curve", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let h = c.borrow().props.get(&(n, "curve")).cloned();
        match h {
            Some(Prop::Handle(h)) => cx.ret(Arg::Handle(h)),
            _ => cx.ret(Arg::Null),
        }
    });
    setter(reg, rt, WS, "curve", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        if cx.arg_kind(0) == ValueKind::Null {
            let mut cc = c.borrow_mut();
            cc.props.remove(&(n, "curve"));
            if let Kind::WaveShaper { curve } = &mut cc.graph.nodes[n].kind {
                *curve = None;
            }
            return Ok(());
        }
        if cx.arg_view_kind(0) != Some(ViewKind::Float32) {
            return Err(HostError::type_error(
                "Failed to set the 'curve' property on 'WaveShaperNode': The provided value is not of type 'Float32Array'.",
            ));
        }
        let v = arg_floats(cx, 0).unwrap_or_default();
        if v.len() < 2 {
            return Err(w.rt.err(
                "InvalidAccessError",
                format!("Failed to set the 'curve' property on 'WaveShaperNode': The curve length provided ({}) is less than the minimum bound (2).", v.len()),
            ));
        }
        let h = cx.make_handle(Arg::Float32Array(v.clone()))?;
        let mut cc = c.borrow_mut();
        cc.props.insert((n, "curve"), Prop::Handle(h));
        if let Kind::WaveShaper { curve } = &mut cc.graph.nodes[n].kind {
            *curve = Some(v);
        }
        Ok(())
    });
    prop_attr(reg, rt, WS, "oversample", Prop::Str("none".into()));
    // Panner / convolver / script processor attributes.
    for (attr, def) in [
        ("panningModel", Prop::Str("equalpower".into())),
        ("distanceModel", Prop::Str("inverse".into())),
        ("refDistance", Prop::Num(1.0)),
        ("maxDistance", Prop::Num(10000.0)),
        ("rolloffFactor", Prop::Num(1.0)),
        ("coneInnerAngle", Prop::Num(360.0)),
        ("coneOuterAngle", Prop::Num(360.0)),
        ("coneOuterGain", Prop::Num(0.0)),
    ] {
        prop_attr(reg, rt, "PannerNode", attr, def);
    }
    prop_attr(reg, rt, "ConvolverNode", "normalize", Prop::Bool(true));
    getter(reg, rt, "ConvolverNode", "buffer", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let h = c.borrow().props.get(&(n, "buffer")).cloned();
        match h {
            Some(Prop::Num(id)) => cx.ret(w.obj_arg(id as u32)),
            _ => cx.ret(Arg::Null),
        }
    });
    setter(reg, rt, "ConvolverNode", "buffer", |cx, w| {
        let (c, n) = this_node(cx, w)?;
        match arg_a(cx, w, 0) {
            Some((id, AudioObj::Buffer(_))) => {
                c.borrow_mut()
                    .props
                    .insert((n, "buffer"), Prop::Num(f64::from(id)));
            }
            _ => {
                c.borrow_mut().props.remove(&(n, "buffer"));
            }
        }
        Ok(())
    });
    prop_attr(
        reg,
        rt,
        "ScriptProcessorNode",
        "bufferSize",
        Prop::Num(1024.0),
    );
}

/// A plain stored attribute with a default.
fn prop_attr(reg: &mut Registry, rt: &Runtime, iface: &'static str, attr: &'static str, def: Prop) {
    let d2 = def.clone();
    getter(reg, rt, iface, attr, move |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = c
            .borrow()
            .props
            .get(&(n, attr))
            .cloned()
            .unwrap_or(d2.clone());
        match v {
            Prop::Num(x) => cx.ret(x),
            Prop::Str(s) => cx.ret(s),
            Prop::Bool(b) => cx.ret(b),
            Prop::Handle(h) => cx.ret(Arg::Handle(h)),
        }
    });
    setter(reg, rt, iface, attr, move |cx, w| {
        let (c, n) = this_node(cx, w)?;
        let v = match &def {
            Prop::Num(_) => Prop::Num(cx.arg_f64(0)?),
            Prop::Str(_) => Prop::Str(cx.arg_string(0)?),
            Prop::Bool(_) => Prop::Bool(cx.arg_bool(0)),
            Prop::Handle(_) => return Ok(()),
        };
        c.borrow_mut().props.insert((n, attr), v);
        Ok(())
    });
}

fn this_buf(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<Rc<RefCell<Buf>>, HostError> {
    match this_a(cx, w)? {
        AudioObj::Buffer(b) => Ok(b),
        _ => Err(illegal()),
    }
}

fn register_buffer(reg: &mut Registry, rt: &Runtime) {
    const B: &str = "AudioBuffer";
    ctor(reg, rt, B, |cx, w| {
        need_args(cx, 1, B, B).map_err(|_| {
            HostError::type_error(
                "Failed to construct 'AudioBuffer': 1 argument required, but only 0 present.",
            )
        })?;
        let d = Dict::arg(cx, 0);
        let ch = d.number(cx, "numberOfChannels")?.unwrap_or(1.0).trunc();
        let len = d.number(cx, "length")?;
        let sr = d.number(cx, "sampleRate")?;
        d.release(cx);
        let (Some(len), Some(sr)) = (len, sr) else {
            return Err(HostError::type_error(
                "Failed to construct 'AudioBuffer': Failed to read the 'length' property from 'AudioBufferOptions': Required member is undefined.",
            ));
        };
        if let Some(e) = buffer_error(w, B, ch, len.trunc(), sr) {
            return Err(e);
        }
        let len = len as usize;
        let id = new_buffer(w, sr as f32, len, vec![vec![0.0; len]; ch as usize]);
        cx.init_this(HostRef::new(K_OBJ, id))
    });
    getter(reg, rt, B, "length", |cx, w| {
        let b = this_buf(cx, w)?;
        let l = b.borrow().length;
        cx.ret(l as f64)
    });
    getter(reg, rt, B, "duration", |cx, w| {
        let b = this_buf(cx, w)?;
        let b = b.borrow();
        cx.ret(b.length as f64 / f64::from(b.sample_rate))
    });
    getter(reg, rt, B, "sampleRate", |cx, w| {
        let b = this_buf(cx, w)?;
        let sr = b.borrow().sample_rate;
        cx.ret(f64::from(sr))
    });
    getter(reg, rt, B, "numberOfChannels", |cx, w| {
        let b = this_buf(cx, w)?;
        let n = b.borrow().data.len();
        cx.ret(n as u32)
    });
    method(reg, rt, B, "getChannelData", |cx, w| {
        need_args(cx, 1, "getChannelData", B)?;
        let b = this_buf(cx, w)?;
        let ch = cx.arg_u32(0)? as usize;
        let n = b.borrow().data.len();
        if ch >= n {
            return Err(w.rt.err(
                "IndexSizeError",
                format!("Failed to execute 'getChannelData' on 'AudioBuffer': channel index ({ch}) exceeds number of channels ({n})"),
            ));
        }
        let existing = b.borrow().views[ch];
        let h = match existing {
            Some(h) => h,
            None => {
                let data = b.borrow().data[ch].clone();
                let h = cx.make_handle(Arg::Float32Array(data))?;
                b.borrow_mut().views[ch] = Some(h);
                h
            }
        };
        cx.ret(Arg::Handle(h))
    });
    method(reg, rt, B, "copyFromChannel", |cx, w| {
        need_args(cx, 2, "copyFromChannel", B)?;
        let b = this_buf(cx, w)?;
        if cx.arg_view_kind(0) != Some(ViewKind::Float32) {
            return Err(type_err(
                "copyFromChannel",
                B,
                "parameter 1 is not of type 'Float32Array'.",
            ));
        }
        let ch = cx.arg_u32(1)? as usize;
        let start = if cx.len() > 2 {
            cx.arg_u32(2)? as usize
        } else {
            0
        };
        let data = buffer_data(cx, &b);
        let Some(src) = data.get(ch) else {
            return Err(w.rt.err(
                "IndexSizeError",
                format!("Failed to execute 'copyFromChannel' on 'AudioBuffer': The channelNumber provided ({ch}) is outside the range [0, {}].", data.len().saturating_sub(1)),
            ));
        };
        let slice = &src[start.min(src.len())..];
        cx.write_arg_bytes(0, 0, &f32s_to_bytes(slice));
        Ok(())
    });
    method(reg, rt, B, "copyToChannel", |cx, w| {
        need_args(cx, 2, "copyToChannel", B)?;
        let b = this_buf(cx, w)?;
        if cx.arg_view_kind(0) != Some(ViewKind::Float32) {
            return Err(type_err(
                "copyToChannel",
                B,
                "parameter 1 is not of type 'Float32Array'.",
            ));
        }
        let src = arg_floats(cx, 0).unwrap_or_default();
        let ch = cx.arg_u32(1)? as usize;
        let start = if cx.len() > 2 {
            cx.arg_u32(2)? as usize
        } else {
            0
        };
        let n = b.borrow().data.len();
        if ch >= n {
            return Err(w.rt.err(
                "IndexSizeError",
                format!("Failed to execute 'copyToChannel' on 'AudioBuffer': The channelNumber provided ({ch}) is outside the range [0, {}].", n - 1),
            ));
        }
        let len = b.borrow().length;
        if start >= len {
            return Ok(());
        }
        let take = src.len().min(len - start);
        let view = b.borrow().views[ch];
        match view {
            Some(h) => {
                cx.write_handle_bytes(h, start * 4, &f32s_to_bytes(&src[..take]));
            }
            None => {
                b.borrow_mut().data[ch][start..start + take].copy_from_slice(&src[..take]);
            }
        }
        Ok(())
    });
}
