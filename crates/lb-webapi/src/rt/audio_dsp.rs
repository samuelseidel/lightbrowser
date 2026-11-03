//! Web Audio DSP without any engine types: the node graph, render quanta,
//! `AudioParam` timelines and the node kernels behind `rt::audio`.
//!
//! Kernels follow Blink where fingerprint scripts look:
//! - Oscillator: band-limited wavetables from a `PeriodicWave` (Blink's
//!   table size, 36 pitch ranges at 400 cents, partial culling, peak
//!   normalisation of range 0, linear interpolation in and between
//!   tables).
//! - DynamicsCompressor: Blink's `DynamicsCompressorKernel` (knee curve,
//!   `k` search, adaptive release polynomial, 32-frame divisions, 6 ms
//!   pre-delay, makeup gain, metering for `reduction`), in `f32` like the
//!   original.
//! - Analyser: Blackman window, FFT, `1/N` magnitude with smoothing, dB
//!   and byte conversions as in Blink's `RealtimeAnalyser`.
//! - Biquad: the Web Audio spec (Audio EQ Cookbook) coefficients with
//!   `f64` state.
//!
//! Rendering is pull-based per 128-frame quantum; cycles read the previous
//! quantum. Output noise (persona audio seed) is applied by the caller
//! with [`apply_noise`].

#![allow(clippy::too_many_arguments)]

use std::f64::consts::PI;

use lb_persona::sfc32::Sfc32;

/// Frames per render quantum.
pub const QUANTUM: usize = 128;

// ---------------------------------------------------------------------------
// FFT
// ---------------------------------------------------------------------------

/// In-place radix-2 complex FFT (`re.len()` must be a power of two).
/// Forward: `X[k] = sum x[n] e^{-2 pi i nk/N}`; inverse without `1/N`.
pub fn fft(re: &mut [f64], im: &mut [f64], inverse: bool) {
    let n = re.len();
    if n <= 1 {
        return;
    }
    let mut j = 0;
    for i in 1..n {
        let mut bit = n >> 1;
        while j & bit != 0 {
            j ^= bit;
            bit >>= 1;
        }
        j |= bit;
        if i < j {
            re.swap(i, j);
            im.swap(i, j);
        }
    }
    let sign = if inverse { 1.0 } else { -1.0 };
    let mut len = 2;
    while len <= n {
        let ang = sign * 2.0 * PI / len as f64;
        let (wr, wi) = (ang.cos(), ang.sin());
        for start in (0..n).step_by(len) {
            let (mut cr, mut ci) = (1.0f64, 0.0f64);
            for k in 0..len / 2 {
                let a = start + k;
                let b = a + len / 2;
                let tr = re[b] * cr - im[b] * ci;
                let ti = re[b] * ci + im[b] * cr;
                re[b] = re[a] - tr;
                im[b] = im[a] - ti;
                re[a] += tr;
                im[a] += ti;
                let nr = cr * wr - ci * wi;
                ci = cr * wi + ci * wr;
                cr = nr;
            }
        }
        len <<= 1;
    }
}

// ---------------------------------------------------------------------------
// PeriodicWave
// ---------------------------------------------------------------------------

/// Oscillator wave shapes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    Sine,
    Square,
    Sawtooth,
    Triangle,
    Custom,
}

impl Shape {
    pub fn parse(s: &str) -> Option<Shape> {
        Some(match s {
            "sine" => Shape::Sine,
            "square" => Shape::Square,
            "sawtooth" => Shape::Sawtooth,
            "triangle" => Shape::Triangle,
            "custom" => Shape::Custom,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            Shape::Sine => "sine",
            Shape::Square => "square",
            Shape::Sawtooth => "sawtooth",
            Shape::Triangle => "triangle",
            Shape::Custom => "custom",
        }
    }
}

/// Band-limited wavetables (Blink `PeriodicWave`).
#[derive(Debug)]
pub struct PeriodicWave {
    size: usize,
    tables: Vec<Vec<f32>>,
    lowest_fundamental: f32,
    rate_scale: f32,
    cents_per_range: f32,
}

const OCTAVE_BANDS: f32 = 3.0;

impl PeriodicWave {
    fn wave_size(sample_rate: f32) -> usize {
        if sample_rate <= 24000.0 {
            2048
        } else if sample_rate <= 88200.0 {
            4096
        } else {
            16384
        }
    }

    /// The built-in shapes.
    pub fn basic(sample_rate: f32, shape: Shape) -> PeriodicWave {
        let size = Self::wave_size(sample_rate);
        let half = size / 2;
        let real = vec![0.0f32; half];
        let mut imag = vec![0.0f32; half];
        for (n, b) in imag.iter_mut().enumerate().skip(1) {
            let pi_factor = 2.0 / (n as f32 * std::f32::consts::PI);
            *b = match shape {
                Shape::Sine | Shape::Custom => {
                    if n == 1 {
                        1.0
                    } else {
                        0.0
                    }
                }
                Shape::Square => pi_factor * if n & 1 == 1 { 2.0 } else { 0.0 },
                Shape::Sawtooth => pi_factor * if n & 1 == 1 { 1.0 } else { -1.0 },
                Shape::Triangle => {
                    if n & 1 == 1 {
                        let pi2 = std::f32::consts::PI * std::f32::consts::PI;
                        let s = if ((n - 1) >> 1) & 1 == 1 { -1.0 } else { 1.0 };
                        8.0 / (pi2 * (n * n) as f32) * s
                    } else {
                        0.0
                    }
                }
            };
        }
        Self::new(sample_rate, &real, &imag, false)
    }

    /// From Fourier coefficients (`createPeriodicWave`).
    pub fn new(sample_rate: f32, real: &[f32], imag: &[f32], disable_normalization: bool) -> Self {
        let size = Self::wave_size(sample_rate);
        let half = size / 2;
        let ranges = (OCTAVE_BANDS * (size as f32).log2()).round() as usize;
        let cents_per_range = 1200.0 / OCTAVE_BANDS;
        let ncomp = real.len().min(imag.len()).min(half);
        let mut tables = Vec::with_capacity(ranges);
        let mut norm = 1.0f32;
        for r in 0..ranges {
            let cull = r as f32 * cents_per_range;
            let partials = (2f32.powf(-cull / 1200.0) * half as f32) as usize;
            let keep = ncomp.min(partials + 1);
            let mut re = vec![0.0f64; size];
            let mut im = vec![0.0f64; size];
            for k in 1..keep {
                // x[t] = sum a cos + b sin; as an inverse FFT of the
                // conjugate-symmetric spectrum X[k] = (a - i b) / 2.
                re[k] = f64::from(real[k]) * 0.5;
                im[k] = -f64::from(imag[k]) * 0.5;
                re[size - k] = re[k];
                im[size - k] = -im[k];
            }
            fft(&mut re, &mut im, true);
            let mut data: Vec<f32> = re.iter().map(|v| *v as f32).collect();
            if !disable_normalization && r == 0 {
                let max = data.iter().fold(0.0f32, |m, v| m.max(v.abs()));
                if max != 0.0 {
                    norm = 1.0 / max;
                }
            }
            for v in &mut data {
                *v *= norm;
            }
            tables.push(data);
        }
        PeriodicWave {
            size,
            tables,
            lowest_fundamental: (sample_rate / 2.0) / half as f32,
            rate_scale: size as f32 / sample_rate,
            cents_per_range,
        }
    }

    fn tables_for(&self, f: f32) -> (&[f32], &[f32], f32) {
        let f = f.abs();
        let ratio = if f > 0.0 {
            f / self.lowest_fundamental
        } else {
            0.5
        };
        let cents = ratio.log2() * 1200.0;
        let n = self.tables.len();
        let pitch = (1.0 + cents / self.cents_per_range)
            .max(0.0)
            .min((n - 1) as f32);
        let r1 = pitch as usize;
        let r2 = if r1 < n - 1 { r1 + 1 } else { r1 };
        (&self.tables[r2], &self.tables[r1], pitch - r1 as f32)
    }

    /// One sample at table position `idx` (0..size) for frequency `f`.
    fn sample(&self, f: f32, idx: f64) -> f32 {
        let (lower, higher, tf) = self.tables_for(f);
        let mask = self.size - 1;
        let i1 = idx as usize;
        let frac = (idx - i1 as f64) as f32;
        let (a, b) = (i1 & mask, (i1 + 1) & mask);
        let hi = (1.0 - frac) * higher[a] + frac * higher[b];
        let lo = (1.0 - frac) * lower[a] + frac * lower[b];
        (1.0 - tf) * hi + tf * lo
    }
}

// ---------------------------------------------------------------------------
// AudioParam
// ---------------------------------------------------------------------------

/// An automation event.
#[derive(Clone, Debug)]
pub enum Event {
    Set { t: f64, v: f32 },
    Linear { t: f64, v: f32 },
    Exp { t: f64, v: f32 },
    Target { t: f64, v: f32, tau: f64 },
    Curve { t: f64, dur: f64, curve: Vec<f32> },
}

impl Event {
    pub fn time(&self) -> f64 {
        match self {
            Event::Set { t, .. }
            | Event::Linear { t, .. }
            | Event::Exp { t, .. }
            | Event::Target { t, .. }
            | Event::Curve { t, .. } => *t,
        }
    }
}

/// An `AudioParam`.
#[derive(Clone, Debug)]
pub struct Param {
    /// Intrinsic value (`.value`), stored as `f32` like Blink.
    pub value: f32,
    pub default: f32,
    pub min: f32,
    pub max: f32,
    pub a_rate: bool,
    pub events: Vec<Event>,
    /// Nodes connected to this param (summed into it, a-rate).
    pub inputs: Vec<usize>,
}

impl Param {
    pub fn new(default: f32, min: f32, max: f32, a_rate: bool) -> Param {
        Param {
            value: default,
            default,
            min,
            max,
            a_rate,
            events: Vec::new(),
            inputs: Vec::new(),
        }
    }

    pub fn insert(&mut self, e: Event) {
        let t = e.time();
        let pos = self
            .events
            .iter()
            .position(|x| x.time() > t)
            .unwrap_or(self.events.len());
        self.events.insert(pos, e);
    }

    pub fn cancel(&mut self, t: f64) {
        self.events.retain(|e| e.time() < t);
    }

    /// Timeline value at time `t` (no audio-rate inputs).
    pub fn at(&self, t: f64) -> f32 {
        let mut prev_t = 0.0f64;
        let mut prev_v = self.value;
        let mut i = 0;
        while i < self.events.len() {
            let e = &self.events[i];
            match e {
                Event::Set { t: t0, v } => {
                    if t < *t0 {
                        return prev_v;
                    }
                    prev_t = *t0;
                    prev_v = *v;
                }
                Event::Linear { t: t1, v } => {
                    if t < *t1 {
                        if *t1 <= prev_t {
                            return *v;
                        }
                        let k = ((t - prev_t) / (t1 - prev_t)).clamp(0.0, 1.0) as f32;
                        return prev_v + (v - prev_v) * k;
                    }
                    prev_t = *t1;
                    prev_v = *v;
                }
                Event::Exp { t: t1, v } => {
                    if t < *t1 {
                        if prev_v == 0.0 || prev_v.signum() != v.signum() || *t1 <= prev_t {
                            return prev_v;
                        }
                        let k = ((t - prev_t) / (t1 - prev_t)).clamp(0.0, 1.0);
                        return (f64::from(prev_v) * (f64::from(*v) / f64::from(prev_v)).powf(k))
                            as f32;
                    }
                    prev_t = *t1;
                    prev_v = *v;
                }
                Event::Target { t: t0, v, tau } => {
                    if t < *t0 {
                        return prev_v;
                    }
                    let end = self.events.get(i + 1).map(Event::time);
                    let upto = match end {
                        Some(te) if te <= t => te,
                        _ => t,
                    };
                    let val = if *tau <= 0.0 {
                        *v
                    } else {
                        (f64::from(*v)
                            + (f64::from(prev_v) - f64::from(*v)) * (-(upto - t0) / tau).exp())
                            as f32
                    };
                    if upto == t {
                        return val;
                    }
                    prev_t = upto;
                    prev_v = val;
                }
                Event::Curve { t: t0, dur, curve } => {
                    if t < *t0 {
                        return prev_v;
                    }
                    let last = curve.last().copied().unwrap_or(prev_v);
                    if t < t0 + dur && curve.len() > 1 {
                        let pos = (t - t0) / dur * (curve.len() - 1) as f64;
                        let k = pos.floor() as usize;
                        let f = (pos - k as f64) as f32;
                        let a = curve[k.min(curve.len() - 1)];
                        let b = curve[(k + 1).min(curve.len() - 1)];
                        return a + (b - a) * f;
                    }
                    prev_t = t0 + dur;
                    prev_v = last;
                }
            }
            i += 1;
        }
        prev_v
    }

    fn clamp(&self, v: f32) -> f32 {
        if v.is_nan() {
            return self.default;
        }
        v.clamp(self.min, self.max)
    }
}

// ---------------------------------------------------------------------------
// Dynamics compressor (Blink DynamicsCompressorKernel)
// ---------------------------------------------------------------------------

const PRE_DELAY_MAX: usize = 1024;
const PRE_DELAY_MASK: usize = PRE_DELAY_MAX - 1;
const DEFAULT_PRE_DELAY: usize = 256;
const DIVISION_FRAMES: usize = 32;
const PI_OVER_TWO_F: f32 = std::f32::consts::FRAC_PI_2;

fn lin_to_db(x: f32) -> f32 {
    20.0 * x.log10()
}

fn db_to_lin(db: f32) -> f32 {
    10f32.powf(0.05 * db)
}

fn flush_denormal(x: f32) -> f32 {
    if x.abs() < f32::MIN_POSITIVE { 0.0 } else { x }
}

/// Blink's compressor kernel.
#[derive(Debug, Clone)]
pub struct Compressor {
    sample_rate: f32,
    pre_delay: Vec<Vec<f32>>,
    last_pre_delay_frames: usize,
    read: usize,
    write: usize,
    detector_average: f32,
    compressor_gain: f32,
    max_attack_db: f32,
    metering_gain: f32,
    meter_release_rate: f32,
    // Static curve cache.
    ratio: f32,
    slope: f32,
    linear_threshold: f32,
    db_threshold: f32,
    db_knee: f32,
    knee_threshold: f32,
    knee_threshold_db: f32,
    yknee_threshold_db: f32,
    k: f32,
}

impl Compressor {
    pub fn new(sample_rate: f32, channels: usize) -> Compressor {
        Compressor {
            sample_rate,
            pre_delay: vec![vec![0.0; PRE_DELAY_MAX]; channels],
            last_pre_delay_frames: DEFAULT_PRE_DELAY,
            read: 0,
            write: DEFAULT_PRE_DELAY,
            detector_average: 0.0,
            compressor_gain: 1.0,
            max_attack_db: -1.0,
            metering_gain: 1.0,
            meter_release_rate: 1.0 - (-1.0 / (sample_rate * 0.325)).exp(),
            ratio: -1.0,
            slope: -1.0,
            linear_threshold: -1.0,
            db_threshold: -1.0,
            db_knee: -1.0,
            knee_threshold: -1.0,
            knee_threshold_db: -1.0,
            yknee_threshold_db: -1.0,
            k: -1.0,
        }
    }

    /// `reduction` in dB.
    pub fn metering_gain(&self) -> f32 {
        self.metering_gain
    }

    fn knee_curve(&self, x: f32, k: f32) -> f32 {
        if x < self.linear_threshold {
            return x;
        }
        self.linear_threshold + (1.0 - (-k * (x - self.linear_threshold)).exp()) / k
    }

    fn saturate(&self, x: f32, k: f32) -> f32 {
        if x < self.knee_threshold {
            self.knee_curve(x, k)
        } else {
            let x_db = lin_to_db(x);
            let y_db = self.yknee_threshold_db + self.slope * (x_db - self.knee_threshold_db);
            db_to_lin(y_db)
        }
    }

    fn slope_at(&self, x: f32, k: f32) -> f32 {
        if x < self.linear_threshold {
            return 1.0;
        }
        let x2 = x * 1.001;
        let x_db = lin_to_db(x);
        let x2_db = lin_to_db(x2);
        let y_db = lin_to_db(self.knee_curve(x, k));
        let y2_db = lin_to_db(self.knee_curve(x2, k));
        (y2_db - y_db) / (x2_db - x_db)
    }

    fn k_at_slope(&self, desired: f32) -> f32 {
        let x = db_to_lin(self.db_threshold + self.db_knee);
        let (mut min_k, mut max_k, mut k) = (0.1f32, 10000.0f32, 5.0f32);
        for _ in 0..15 {
            if self.slope_at(x, k) < desired {
                max_k = k;
            } else {
                min_k = k;
            }
            k = (min_k * max_k).sqrt();
        }
        k
    }

    fn update_curve(&mut self, db_threshold: f32, db_knee: f32, ratio: f32) -> f32 {
        if db_threshold != self.db_threshold || db_knee != self.db_knee || ratio != self.ratio {
            self.db_threshold = db_threshold;
            self.linear_threshold = db_to_lin(db_threshold);
            self.db_knee = db_knee;
            self.ratio = ratio;
            self.slope = 1.0 / ratio;
            let k = self.k_at_slope(1.0 / ratio);
            self.knee_threshold_db = db_threshold + db_knee;
            self.knee_threshold = db_to_lin(self.knee_threshold_db);
            self.yknee_threshold_db = lin_to_db(self.knee_curve(self.knee_threshold, k));
            self.k = k;
        }
        self.k
    }

    fn set_pre_delay(&mut self, t: f32) {
        let frames = ((t * self.sample_rate) as usize).min(PRE_DELAY_MAX - 1);
        if frames != self.last_pre_delay_frames {
            self.last_pre_delay_frames = frames;
            for b in &mut self.pre_delay {
                b.iter_mut().for_each(|v| *v = 0.0);
            }
            self.read = 0;
            self.write = frames;
        }
    }

    /// Process `frames` (a multiple of 32) of every channel.
    // The release-curve coefficients are Blink's, digit for digit.
    #[allow(clippy::excessive_precision, clippy::needless_range_loop)]
    pub fn process(
        &mut self,
        src: &[Vec<f32>],
        dst: &mut [Vec<f32>],
        frames: usize,
        db_threshold: f32,
        db_knee: f32,
        ratio: f32,
        attack: f32,
        release: f32,
    ) {
        const PRE_DELAY_TIME: f32 = 0.006;
        const RELEASE_ZONES: [f32; 4] = [0.09, 0.16, 0.42, 0.98];
        let sr = self.sample_rate;
        let (dry_mix, wet_mix) = (0.0f32, 1.0f32);
        let k = self.update_curve(db_threshold, db_knee, ratio);
        let full_range_gain = self.saturate(1.0, k);
        let makeup = (1.0 / full_range_gain).powf(0.6);
        let master_gain = db_to_lin(0.0) * makeup;
        let attack_frames = attack.max(0.001) * sr;
        let release_frames = sr * release;
        let sat_release_frames = 0.0025f32 * sr;
        let [y1, y2, y3, y4] = RELEASE_ZONES.map(|z| release_frames * z);
        let ka = 0.999_999_999_999_999_8_f32 * y1 + 1.843_221_968_432_392_3e-16_f32 * y2
            - 1.937_339_435_167_642_3e-16_f32 * y3
            + 8.824_516_011_816_245e-18_f32 * y4;
        let kb = -1.578_832_035_284_588_8_f32 * y1 + 2.330_583_703_207_428_6_f32 * y2
            - 0.914_119_420_484_042_9_f32 * y3
            + 0.162_367_752_561_203_2_f32 * y4;
        let kc = 0.533_414_286_910_642_4_f32 * y1 - 1.272_736_789_213_631_f32 * y2
            + 0.925_885_604_220_751_2_f32 * y3
            - 0.186_563_101_917_762_26_f32 * y4;
        let kd = 0.087_834_631_382_072_34_f32 * y1 - 0.169_416_296_792_562_2_f32 * y2
            + 0.085_880_579_515_952_72_f32 * y3
            - 0.004_298_914_105_462_83_f32 * y4;
        let ke = -0.042_416_883_008_123_074_f32 * y1 + 0.111_569_382_798_760_2_f32 * y2
            - 0.097_646_763_252_658_72_f32 * y3
            + 0.028_494_263_462_021_576_f32 * y4;
        self.set_pre_delay(PRE_DELAY_TIME);
        let channels = src.len().min(dst.len()).min(self.pre_delay.len());
        let mut frame = 0usize;
        for _ in 0..frames / DIVISION_FRAMES {
            if !self.detector_average.is_finite() {
                self.detector_average = 1.0;
            }
            let desired_gain = self.detector_average;
            let scaled_desired_gain = desired_gain.asin() / PI_OVER_TWO_F;
            let is_releasing = scaled_desired_gain > self.compressor_gain;
            let mut diff_db = lin_to_db(self.compressor_gain / scaled_desired_gain);
            let envelope_rate = if is_releasing {
                self.max_attack_db = -1.0;
                if !diff_db.is_finite() {
                    diff_db = -1.0;
                }
                let x = 0.25 * (diff_db.clamp(-12.0, 0.0) + 12.0);
                let x2 = x * x;
                let x3 = x2 * x;
                let x4 = x2 * x2;
                let rel = ka + kb * x + kc * x2 + kd * x3 + ke * x4;
                db_to_lin(5.0 / rel)
            } else {
                if !diff_db.is_finite() {
                    diff_db = 1.0;
                }
                if self.max_attack_db == -1.0 || self.max_attack_db < diff_db {
                    self.max_attack_db = diff_db;
                }
                let eff = self.max_attack_db.max(0.5);
                let x = 0.25 / eff;
                1.0 - x.powf(1.0 / attack_frames)
            };
            let mut read = self.read;
            let mut write = self.write;
            let mut detector_average = self.detector_average;
            let mut compressor_gain = self.compressor_gain;
            for _ in 0..DIVISION_FRAMES {
                let mut input = 0.0f32;
                for c in 0..channels {
                    let s = src[c].get(frame).copied().unwrap_or(0.0);
                    self.pre_delay[c][write] = s;
                    let a = s.abs();
                    if input < a {
                        input = a;
                    }
                }
                let abs_input = input.abs();
                let shaped = self.saturate(abs_input, k);
                let attenuation = if abs_input <= 0.0001 {
                    1.0
                } else {
                    shaped / abs_input
                };
                let att_db = (-lin_to_db(attenuation)).max(2.0);
                let db_per_frame = att_db / sat_release_frames;
                let sat_release_rate = db_to_lin(db_per_frame) - 1.0;
                let rate = if attenuation > detector_average {
                    sat_release_rate
                } else {
                    1.0
                };
                detector_average += (attenuation - detector_average) * rate;
                detector_average = detector_average.min(1.0);
                if !detector_average.is_finite() {
                    detector_average = 1.0;
                }
                if envelope_rate < 1.0 {
                    compressor_gain += (scaled_desired_gain - compressor_gain) * envelope_rate;
                } else {
                    compressor_gain *= envelope_rate;
                    compressor_gain = compressor_gain.min(1.0);
                }
                let post_warp = (PI_OVER_TWO_F * compressor_gain).sin();
                let total_gain = dry_mix + wet_mix * master_gain * post_warp;
                let db_real_gain = 20.0 * post_warp.log10();
                if db_real_gain < self.metering_gain {
                    self.metering_gain = db_real_gain;
                } else {
                    self.metering_gain +=
                        (db_real_gain - self.metering_gain) * self.meter_release_rate;
                }
                for c in 0..channels {
                    if let Some(d) = dst[c].get_mut(frame) {
                        *d = self.pre_delay[c][read] * total_gain;
                    }
                }
                frame += 1;
                read = (read + 1) & PRE_DELAY_MASK;
                write = (write + 1) & PRE_DELAY_MASK;
            }
            self.read = read;
            self.write = write;
            self.detector_average = flush_denormal(detector_average);
            self.compressor_gain = flush_denormal(compressor_gain);
        }
    }
}

// ---------------------------------------------------------------------------
// Biquad
// ---------------------------------------------------------------------------

/// Biquad filter types.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BiquadType {
    Lowpass,
    Highpass,
    Bandpass,
    Lowshelf,
    Highshelf,
    Peaking,
    Notch,
    Allpass,
}

impl BiquadType {
    pub fn parse(s: &str) -> Option<BiquadType> {
        Some(match s {
            "lowpass" => BiquadType::Lowpass,
            "highpass" => BiquadType::Highpass,
            "bandpass" => BiquadType::Bandpass,
            "lowshelf" => BiquadType::Lowshelf,
            "highshelf" => BiquadType::Highshelf,
            "peaking" => BiquadType::Peaking,
            "notch" => BiquadType::Notch,
            "allpass" => BiquadType::Allpass,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            BiquadType::Lowpass => "lowpass",
            BiquadType::Highpass => "highpass",
            BiquadType::Bandpass => "bandpass",
            BiquadType::Lowshelf => "lowshelf",
            BiquadType::Highshelf => "highshelf",
            BiquadType::Peaking => "peaking",
            BiquadType::Notch => "notch",
            BiquadType::Allpass => "allpass",
        }
    }
}

/// Normalised coefficients `[b0, b1, b2, a1, a2]` (spec formulas).
pub fn biquad_coefficients(
    ty: BiquadType,
    sample_rate: f64,
    freq: f64,
    q: f64,
    gain: f64,
    detune: f64,
) -> [f64; 5] {
    let nyquist = sample_rate / 2.0;
    let f0 = (freq * 2f64.powf(detune / 1200.0)).clamp(0.0, nyquist);
    let w0 = 2.0 * PI * f0 / sample_rate;
    let a = 10f64.powf(gain / 40.0);
    let (sin, cos) = (w0.sin(), w0.cos());
    let alpha_q = sin / (2.0 * q);
    let alpha_qdb = sin / (2.0 * 10f64.powf(q / 20.0));
    let s = 1.0;
    let alpha_s = sin / 2.0 * ((a + 1.0 / a) * (1.0 / s - 1.0) + 2.0).sqrt();
    let (b0, b1, b2, a0, a1, a2) = match ty {
        BiquadType::Lowpass => (
            (1.0 - cos) / 2.0,
            1.0 - cos,
            (1.0 - cos) / 2.0,
            1.0 + alpha_qdb,
            -2.0 * cos,
            1.0 - alpha_qdb,
        ),
        BiquadType::Highpass => (
            (1.0 + cos) / 2.0,
            -(1.0 + cos),
            (1.0 + cos) / 2.0,
            1.0 + alpha_qdb,
            -2.0 * cos,
            1.0 - alpha_qdb,
        ),
        BiquadType::Bandpass => (
            alpha_q,
            0.0,
            -alpha_q,
            1.0 + alpha_q,
            -2.0 * cos,
            1.0 - alpha_q,
        ),
        BiquadType::Notch => (
            1.0,
            -2.0 * cos,
            1.0,
            1.0 + alpha_q,
            -2.0 * cos,
            1.0 - alpha_q,
        ),
        BiquadType::Allpass => (
            1.0 - alpha_q,
            -2.0 * cos,
            1.0 + alpha_q,
            1.0 + alpha_q,
            -2.0 * cos,
            1.0 - alpha_q,
        ),
        BiquadType::Peaking => (
            1.0 + alpha_q * a,
            -2.0 * cos,
            1.0 - alpha_q * a,
            1.0 + alpha_q / a,
            -2.0 * cos,
            1.0 - alpha_q / a,
        ),
        BiquadType::Lowshelf => {
            let sa = 2.0 * a.sqrt() * alpha_s;
            (
                a * ((a + 1.0) - (a - 1.0) * cos + sa),
                2.0 * a * ((a - 1.0) - (a + 1.0) * cos),
                a * ((a + 1.0) - (a - 1.0) * cos - sa),
                (a + 1.0) + (a - 1.0) * cos + sa,
                -2.0 * ((a - 1.0) + (a + 1.0) * cos),
                (a + 1.0) + (a - 1.0) * cos - sa,
            )
        }
        BiquadType::Highshelf => {
            let sa = 2.0 * a.sqrt() * alpha_s;
            (
                a * ((a + 1.0) + (a - 1.0) * cos + sa),
                -2.0 * a * ((a - 1.0) + (a + 1.0) * cos),
                a * ((a + 1.0) + (a - 1.0) * cos - sa),
                (a + 1.0) - (a - 1.0) * cos + sa,
                2.0 * ((a - 1.0) - (a + 1.0) * cos),
                (a + 1.0) - (a - 1.0) * cos - sa,
            )
        }
    };
    if a0 == 0.0 || !a0.is_finite() {
        return [1.0, 0.0, 0.0, 0.0, 0.0];
    }
    [b0 / a0, b1 / a0, b2 / a0, a1 / a0, a2 / a0]
}

/// Magnitude and phase of `H(e^{iw})` for normalised frequency `f`
/// (0..1 of Nyquist) of a biquad or IIR filter.
pub fn response(b: &[f64], a: &[f64], f: f64) -> (f32, f32) {
    let w = PI * f;
    let eval = |c: &[f64]| {
        let (mut re, mut im) = (0.0, 0.0);
        for (k, v) in c.iter().enumerate() {
            re += v * (-(k as f64) * w).cos();
            im += v * (-(k as f64) * w).sin();
        }
        (re, im)
    };
    let (nr, ni) = eval(b);
    let (dr, di) = eval(a);
    let den = dr * dr + di * di;
    let hr = (nr * dr + ni * di) / den;
    let hi = (ni * dr - nr * di) / den;
    ((hr * hr + hi * hi).sqrt() as f32, hi.atan2(hr) as f32)
}

// ---------------------------------------------------------------------------
// Analyser
// ---------------------------------------------------------------------------

/// Largest `fftSize`.
pub const MAX_FFT: usize = 32768;

/// Analyser state (Blink `RealtimeAnalyser`).
#[derive(Debug, Clone)]
pub struct Analyser {
    pub fft_size: usize,
    pub min_db: f64,
    pub max_db: f64,
    pub smoothing: f64,
    ring: Vec<f32>,
    write: usize,
    magnitude: Vec<f32>,
    last_frame: Option<u64>,
}

impl Default for Analyser {
    fn default() -> Self {
        Analyser {
            fft_size: 2048,
            min_db: -100.0,
            max_db: -30.0,
            smoothing: 0.8,
            ring: vec![0.0; MAX_FFT],
            write: 0,
            magnitude: vec![0.0; 1024],
            last_frame: None,
        }
    }
}

impl Analyser {
    pub fn set_fft_size(&mut self, n: usize) {
        self.fft_size = n;
        self.magnitude = vec![0.0; n / 2];
    }

    fn push(&mut self, mono: &[f32]) {
        for v in mono {
            self.ring[self.write] = *v;
            self.write = (self.write + 1) % MAX_FFT;
        }
    }

    /// The last `fft_size` input samples.
    pub fn time_domain(&self) -> Vec<f32> {
        let n = self.fft_size;
        (0..n)
            .map(|i| self.ring[(self.write + MAX_FFT - n + i) % MAX_FFT])
            .collect()
    }

    fn analyse(&mut self, frame: u64) {
        if self.last_frame == Some(frame) {
            return;
        }
        self.last_frame = Some(frame);
        let n = self.fft_size;
        let input = self.time_domain();
        let mut re: Vec<f64> = input
            .iter()
            .enumerate()
            .map(|(i, v)| {
                let x = i as f64 / n as f64;
                let w = 0.42 - 0.5 * (2.0 * PI * x).cos() + 0.08 * (4.0 * PI * x).cos();
                f64::from(*v) * w
            })
            .collect();
        let mut im = vec![0.0; n];
        fft(&mut re, &mut im, false);
        let k = self.smoothing.clamp(0.0, 1.0);
        let scale = 1.0 / n as f64;
        if self.magnitude.len() != n / 2 {
            self.magnitude = vec![0.0; n / 2];
        }
        for (i, m) in self.magnitude.iter_mut().enumerate() {
            let imag = if i == 0 { 0.0 } else { im[i] };
            let mag = (re[i] * re[i] + imag * imag).sqrt() * scale;
            let v = k * f64::from(*m) + (1.0 - k) * mag;
            *m = if v.is_finite() { v as f32 } else { 0.0 };
        }
    }

    /// `getFloatFrequencyData` values (dB) at render frame `frame`.
    pub fn float_frequency(&mut self, frame: u64) -> Vec<f32> {
        self.analyse(frame);
        self.magnitude.iter().map(|m| lin_to_db(*m)).collect()
    }

    /// `getByteFrequencyData` values.
    pub fn byte_frequency(&mut self, frame: u64) -> Vec<u8> {
        self.analyse(frame);
        let range = if self.max_db == self.min_db {
            1.0
        } else {
            1.0 / (self.max_db - self.min_db)
        };
        self.magnitude
            .iter()
            .map(|m| {
                let db = if *m == 0.0 {
                    -1000.0
                } else {
                    f64::from(lin_to_db(*m))
                };
                (255.0 * (db - self.min_db) * range).clamp(0.0, 255.0) as u8
            })
            .collect()
    }
}

/// `getByteTimeDomainData` conversion.
pub fn byte_time_domain(v: &[f32]) -> Vec<u8> {
    v.iter()
        .map(|x| (128.0 * (f64::from(*x) + 1.0)).clamp(0.0, 255.0) as u8)
        .collect()
}

// ---------------------------------------------------------------------------
// Noise
// ---------------------------------------------------------------------------

/// Adds the persona's audio noise to rendered samples: every non-zero
/// sample gets a uniform offset in [-0.0001, 0.0001) from an SFC32 seeded
/// with the audio seed whose bytes 8..12 are XORed with `stream` and
/// 12..16 with the length (big-endian). Same seed, same stream, same
/// input: same output. Silence stays exact.
pub fn apply_noise(seed: [u8; 16], stream: u32, data: &mut [f32]) {
    let mut s = seed;
    for (i, b) in stream.to_be_bytes().into_iter().enumerate() {
        s[8 + i] ^= b;
    }
    for (i, b) in (data.len() as u32).to_be_bytes().into_iter().enumerate() {
        s[12 + i] ^= b;
    }
    let mut rng = Sfc32::new(s);
    for v in data.iter_mut() {
        let r = rng.next_u32();
        if *v != 0.0 && v.is_finite() {
            let n = (f64::from(r) / 4_294_967_296.0 * 2.0 - 1.0) * 1e-4;
            *v = (f64::from(*v) + n) as f32;
        }
    }
}

// ---------------------------------------------------------------------------
// Graph
// ---------------------------------------------------------------------------

/// `channelCountMode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CountMode {
    Max,
    ClampedMax,
    Explicit,
}

impl CountMode {
    pub fn parse(s: &str) -> Option<CountMode> {
        Some(match s {
            "max" => CountMode::Max,
            "clamped-max" => CountMode::ClampedMax,
            "explicit" => CountMode::Explicit,
            _ => return None,
        })
    }
    pub fn as_str(self) -> &'static str {
        match self {
            CountMode::Max => "max",
            CountMode::ClampedMax => "clamped-max",
            CountMode::Explicit => "explicit",
        }
    }
}

/// When a source plays.
#[derive(Clone, Debug, Default)]
pub struct Schedule {
    pub start: Option<f64>,
    pub stop: Option<f64>,
    /// `ended` was reported.
    pub ended: bool,
}

impl Schedule {
    fn frames(&self, sr: f64) -> Option<(u64, u64)> {
        let start = self.start?;
        let s = (start * sr - 1e-9).ceil().max(0.0) as u64;
        let e = self
            .stop
            .map_or(u64::MAX, |t| (t * sr - 1e-9).ceil().max(0.0) as u64);
        Some((s, e))
    }
}

/// Node kernels.
#[derive(Debug)]
pub enum Kind {
    Destination,
    Oscillator {
        shape: Shape,
        wave: Option<std::rc::Rc<PeriodicWave>>,
        custom: Option<std::rc::Rc<PeriodicWave>>,
        phase: f64,
        sched: Schedule,
    },
    Gain,
    Compressor {
        kernel: Box<Compressor>,
        reduction: f32,
    },
    Analyser(Box<Analyser>),
    Biquad {
        ty: BiquadType,
        state: Vec<[f64; 4]>,
    },
    Iir {
        ff: Vec<f64>,
        fb: Vec<f64>,
        state: Vec<(Vec<f64>, Vec<f64>)>,
    },
    BufferSource {
        buffer: Option<std::rc::Rc<Vec<Vec<f32>>>>,
        buffer_rate: f32,
        pos: f64,
        looping: bool,
        loop_start: f64,
        loop_end: f64,
        offset: f64,
        sched: Schedule,
    },
    Constant {
        sched: Schedule,
    },
    Delay {
        max: f64,
        lines: Vec<Vec<f32>>,
        write: usize,
    },
    StereoPanner,
    WaveShaper {
        curve: Option<Vec<f32>>,
    },
    Merger(usize),
    Splitter(usize),
    /// Panner, convolver, script processor, media nodes: up-mixed input.
    PassThrough {
        out_channels: Option<usize>,
    },
}

/// A node.
#[derive(Debug)]
pub struct Node {
    pub kind: Kind,
    /// `(source node, source output, this input)`.
    pub inputs: Vec<(usize, usize, usize)>,
    pub params: Vec<Param>,
    pub channel_count: usize,
    pub mode: CountMode,
    pub discrete: bool,
    out: Vec<Vec<f32>>,
    done: Option<u64>,
    busy: bool,
}

impl Node {
    pub fn new(kind: Kind, params: Vec<Param>, channel_count: usize, mode: CountMode) -> Node {
        Node {
            kind,
            inputs: Vec::new(),
            params,
            channel_count,
            mode,
            discrete: false,
            out: vec![vec![0.0; QUANTUM]],
            done: None,
            busy: false,
        }
    }
}

/// A context's node graph. Node 0 is the destination.
#[derive(Debug)]
pub struct Graph {
    pub sample_rate: f32,
    pub nodes: Vec<Node>,
    /// Frames rendered so far.
    pub frame: u64,
}

fn mix_into(dst: &mut [Vec<f32>], src: &[Vec<f32>], discrete: bool) {
    let (dn, sn) = (dst.len(), src.len());
    if dn == sn {
        for (d, s) in dst.iter_mut().zip(src) {
            for (a, b) in d.iter_mut().zip(s) {
                *a += *b;
            }
        }
    } else if !discrete && sn == 1 && dn >= 2 {
        for d in dst.iter_mut().take(2) {
            for (a, b) in d.iter_mut().zip(&src[0]) {
                *a += *b;
            }
        }
    } else if !discrete && sn == 2 && dn == 1 {
        for i in 0..QUANTUM {
            dst[0][i] += 0.5 * (src[0][i] + src[1][i]);
        }
    } else {
        for (d, s) in dst.iter_mut().zip(src) {
            for (a, b) in d.iter_mut().zip(s) {
                *a += *b;
            }
        }
    }
}

impl Graph {
    pub fn new(sample_rate: f32, channels: usize) -> Graph {
        let mut dest = Node::new(Kind::Destination, Vec::new(), channels, CountMode::Explicit);
        dest.out = vec![vec![0.0; QUANTUM]; channels.max(1)];
        Graph {
            sample_rate,
            nodes: vec![dest],
            frame: 0,
        }
    }

    pub fn add(&mut self, n: Node) -> usize {
        self.nodes.push(n);
        self.nodes.len() - 1
    }

    pub fn time(&self) -> f64 {
        self.frame as f64 / f64::from(self.sample_rate)
    }

    /// Output channels of node `i` for the current quantum.
    fn output(&mut self, i: usize, output: usize) -> Vec<Vec<f32>> {
        self.render(i);
        let n = &self.nodes[i];
        if let Kind::Splitter(_) = n.kind {
            return vec![
                n.out
                    .get(output)
                    .cloned()
                    .unwrap_or_else(|| vec![0.0; QUANTUM]),
            ];
        }
        n.out.clone()
    }

    /// Mixed input of node `i` (`None`: nothing connected).
    fn input(&mut self, i: usize, input_index: usize) -> Option<Vec<Vec<f32>>> {
        let conns: Vec<(usize, usize)> = self.nodes[i]
            .inputs
            .iter()
            .filter(|c| c.2 == input_index)
            .map(|c| (c.0, c.1))
            .collect();
        if conns.is_empty() {
            return None;
        }
        let outs: Vec<Vec<Vec<f32>>> = conns.iter().map(|(s, o)| self.output(*s, *o)).collect();
        let max = outs.iter().map(Vec::len).max().unwrap_or(1).max(1);
        let n = &self.nodes[i];
        let ch = match n.mode {
            CountMode::Max => max,
            CountMode::ClampedMax => max.min(n.channel_count.max(1)),
            CountMode::Explicit => n.channel_count.max(1),
        };
        let mut bus = vec![vec![0.0f32; QUANTUM]; ch];
        for o in &outs {
            mix_into(&mut bus, o, n.discrete);
        }
        Some(bus)
    }

    /// Values of param `p` of node `i` for this quantum (128 values for
    /// a-rate or connected params, else one).
    fn param(&mut self, i: usize, p: usize) -> Vec<f32> {
        let sr = f64::from(self.sample_rate);
        let t0 = self.time();
        let inputs = self.nodes[i].params[p].inputs.clone();
        let mut extra: Option<Vec<f32>> = None;
        for s in inputs {
            let o = self.output(s, 0);
            let e = extra.get_or_insert_with(|| vec![0.0; QUANTUM]);
            for c in &o {
                for (a, b) in e.iter_mut().zip(c) {
                    *a += *b;
                }
            }
            if o.len() > 1 {
                for v in e.iter_mut() {
                    *v /= o.len() as f32;
                }
            }
        }
        let prm = &self.nodes[i].params[p];
        if prm.a_rate || extra.is_some() {
            (0..QUANTUM)
                .map(|k| {
                    let base = if prm.a_rate {
                        prm.at(t0 + k as f64 / sr)
                    } else {
                        prm.at(t0)
                    };
                    prm.clamp(base + extra.as_ref().map_or(0.0, |e| e[k]))
                })
                .collect()
        } else {
            vec![prm.clamp(prm.at(t0))]
        }
    }

    fn render(&mut self, i: usize) {
        if self.nodes[i].done == Some(self.frame) || self.nodes[i].busy {
            return;
        }
        self.nodes[i].busy = true;
        let out = self.process(i);
        let n = &mut self.nodes[i];
        n.out = out;
        n.done = Some(self.frame);
        n.busy = false;
    }

    fn silence(ch: usize) -> Vec<Vec<f32>> {
        vec![vec![0.0; QUANTUM]; ch.max(1)]
    }

    fn process(&mut self, i: usize) -> Vec<Vec<f32>> {
        let sr = self.sample_rate;
        let frame = self.frame;
        match &self.nodes[i].kind {
            Kind::Destination => {
                let ch = self.nodes[i].channel_count.max(1);
                let mut bus = Self::silence(ch);
                if let Some(inp) = self.input(i, 0) {
                    bus = Self::silence(ch);
                    mix_into(&mut bus, &inp, self.nodes[i].discrete);
                }
                bus
            }
            Kind::Gain => {
                let Some(mut bus) = self.input(i, 0) else {
                    return Self::silence(1);
                };
                let g = self.param(i, 0);
                for c in &mut bus {
                    for (k, v) in c.iter_mut().enumerate() {
                        *v *= g[k.min(g.len() - 1)];
                    }
                }
                bus
            }
            Kind::Oscillator { .. } => self.oscillator(i),
            Kind::Constant { sched } => {
                let range = sched.frames(f64::from(sr));
                let off = self.param(i, 0);
                let mut out = vec![0.0f32; QUANTUM];
                if let Some((s, e)) = range {
                    for (k, v) in out.iter_mut().enumerate() {
                        let f = frame + k as u64;
                        if f >= s && f < e {
                            *v = off[k.min(off.len() - 1)];
                        }
                    }
                }
                vec![out]
            }
            Kind::Compressor { .. } => {
                let inp = self.input(i, 0);
                let vals: Vec<f32> = (0..5).map(|p| self.param(i, p)[0]).collect();
                let Kind::Compressor { kernel, reduction } = &mut self.nodes[i].kind else {
                    unreachable!()
                };
                let mut src = inp.unwrap_or_else(|| Self::silence(1));
                if src.len() == 1 {
                    src.push(src[0].clone());
                }
                src.truncate(2);
                let mut dst = Self::silence(2);
                kernel.process(
                    &src, &mut dst, QUANTUM, vals[0], vals[1], vals[2], vals[3], vals[4],
                );
                *reduction = kernel.metering_gain();
                dst
            }
            Kind::Analyser(_) => {
                let inp = self.input(i, 0);
                let bus = inp.unwrap_or_else(|| Self::silence(1));
                let mono: Vec<f32> = if bus.len() == 1 {
                    bus[0].clone()
                } else if bus.len() == 2 {
                    (0..QUANTUM)
                        .map(|k| 0.5 * (bus[0][k] + bus[1][k]))
                        .collect()
                } else {
                    bus[0].clone()
                };
                if let Kind::Analyser(a) = &mut self.nodes[i].kind {
                    a.push(&mono);
                }
                bus
            }
            Kind::Biquad { .. } => {
                let Some(mut bus) = self.input(i, 0) else {
                    return Self::silence(1);
                };
                let f = self.param(i, 0)[0];
                let det = self.param(i, 1)[0];
                let q = self.param(i, 2)[0];
                let g = self.param(i, 3)[0];
                let Kind::Biquad { ty, state } = &mut self.nodes[i].kind else {
                    unreachable!()
                };
                let c = biquad_coefficients(
                    *ty,
                    f64::from(sr),
                    f64::from(f),
                    f64::from(q),
                    f64::from(g),
                    f64::from(det),
                );
                state.resize(bus.len(), [0.0; 4]);
                for (ch, st) in bus.iter_mut().zip(state.iter_mut()) {
                    for v in ch.iter_mut() {
                        let x = f64::from(*v);
                        let y =
                            c[0] * x + c[1] * st[0] + c[2] * st[1] - c[3] * st[2] - c[4] * st[3];
                        st[1] = st[0];
                        st[0] = x;
                        st[3] = st[2];
                        st[2] = y;
                        *v = y as f32;
                    }
                }
                bus
            }
            Kind::Iir { .. } => {
                let Some(mut bus) = self.input(i, 0) else {
                    return Self::silence(1);
                };
                let Kind::Iir { ff, fb, state } = &mut self.nodes[i].kind else {
                    unreachable!()
                };
                let a0 = fb.first().copied().unwrap_or(1.0);
                state.resize(bus.len(), (vec![0.0; ff.len()], vec![0.0; fb.len()]));
                for (ch, (xs, ys)) in bus.iter_mut().zip(state.iter_mut()) {
                    for v in ch.iter_mut() {
                        xs.rotate_right(1);
                        xs[0] = f64::from(*v);
                        let mut y: f64 = ff.iter().zip(xs.iter()).map(|(b, x)| b * x).sum();
                        for k in 1..fb.len() {
                            y -= fb[k] * ys[k - 1];
                        }
                        y /= a0;
                        if !ys.is_empty() {
                            ys.rotate_right(1);
                            ys[0] = y;
                        }
                        *v = y as f32;
                    }
                }
                bus
            }
            Kind::BufferSource { .. } => self.buffer_source(i),
            Kind::Delay { .. } => {
                let inp = self.input(i, 0).unwrap_or_else(|| Self::silence(1));
                let dt = self.param(i, 0);
                let Kind::Delay { max, lines, write } = &mut self.nodes[i].kind else {
                    unreachable!()
                };
                let len = ((*max * f64::from(sr)) as usize + QUANTUM + 2).max(QUANTUM * 2);
                lines.resize(inp.len(), vec![0.0; len]);
                let mut out = Self::silence(inp.len());
                for k in 0..QUANTUM {
                    let w = (*write + k) % len;
                    for (c, line) in lines.iter_mut().enumerate() {
                        line[w] = inp[c][k];
                    }
                    let d = f64::from(dt[k.min(dt.len() - 1)]).clamp(0.0, *max) * f64::from(sr);
                    let pos = w as f64 - d;
                    let pos = if pos < 0.0 { pos + len as f64 } else { pos };
                    let i0 = pos.floor() as usize % len;
                    let i1 = (i0 + 1) % len;
                    let fr = (pos - pos.floor()) as f32;
                    for (c, line) in lines.iter().enumerate() {
                        out[c][k] = line[i0] * (1.0 - fr) + line[i1] * fr;
                    }
                }
                *write = (*write + QUANTUM) % len;
                out
            }
            Kind::StereoPanner => {
                let inp = self.input(i, 0).unwrap_or_else(|| Self::silence(1));
                let pan = self.param(i, 0);
                let mut out = Self::silence(2);
                for k in 0..QUANTUM {
                    let p = f64::from(pan[k.min(pan.len() - 1)]).clamp(-1.0, 1.0);
                    if inp.len() == 1 {
                        let x = (p + 1.0) / 2.0;
                        let (gl, gr) = ((x * PI / 2.0).cos(), (x * PI / 2.0).sin());
                        out[0][k] = (f64::from(inp[0][k]) * gl) as f32;
                        out[1][k] = (f64::from(inp[0][k]) * gr) as f32;
                    } else {
                        let (l, r) = (f64::from(inp[0][k]), f64::from(inp[1][k]));
                        if p <= 0.0 {
                            let x = p + 1.0;
                            let (gl, gr) = ((x * PI / 2.0).cos(), (x * PI / 2.0).sin());
                            out[0][k] = (l + r * gl) as f32;
                            out[1][k] = (r * gr) as f32;
                        } else {
                            let x = p;
                            let (gl, gr) = ((x * PI / 2.0).cos(), (x * PI / 2.0).sin());
                            out[0][k] = (l * gl) as f32;
                            out[1][k] = (r + l * gr) as f32;
                        }
                    }
                }
                out
            }
            Kind::WaveShaper { curve } => {
                let curve = curve.clone();
                let Some(mut bus) = self.input(i, 0) else {
                    return Self::silence(1);
                };
                if let Some(c) = curve.filter(|c| !c.is_empty()) {
                    let n = c.len();
                    for ch in &mut bus {
                        for v in ch.iter_mut() {
                            let pos = (n - 1) as f64 * 0.5 * (f64::from(*v) + 1.0);
                            *v = if pos < 0.0 {
                                c[0]
                            } else if pos >= (n - 1) as f64 {
                                c[n - 1]
                            } else {
                                let k = pos as usize;
                                let f = (pos - k as f64) as f32;
                                (1.0 - f) * c[k] + f * c[k + 1]
                            };
                        }
                    }
                }
                bus
            }
            Kind::Merger(n) => {
                let n = *n;
                let mut out = Self::silence(n);
                for (k, ch) in out.iter_mut().enumerate() {
                    if let Some(b) = self.input(i, k) {
                        let mono: Vec<f32> = if b.len() == 2 {
                            (0..QUANTUM).map(|j| 0.5 * (b[0][j] + b[1][j])).collect()
                        } else {
                            b[0].clone()
                        };
                        *ch = mono;
                    }
                }
                out
            }
            Kind::Splitter(n) => {
                let n = *n;
                let inp = self.input(i, 0).unwrap_or_else(|| Self::silence(1));
                (0..n)
                    .map(|k| inp.get(k).cloned().unwrap_or_else(|| vec![0.0; QUANTUM]))
                    .collect()
            }
            Kind::PassThrough { out_channels } => {
                let want = *out_channels;
                let inp = self.input(i, 0).unwrap_or_else(|| Self::silence(1));
                match want {
                    Some(n) if n != inp.len() => {
                        let mut out = Self::silence(n);
                        mix_into(&mut out, &inp, false);
                        out
                    }
                    _ => inp,
                }
            }
        }
    }

    fn oscillator(&mut self, i: usize) -> Vec<Vec<f32>> {
        let sr = self.sample_rate;
        let frame = self.frame;
        let freq = self.param(i, 0);
        let det = self.param(i, 1);
        let Kind::Oscillator {
            shape,
            wave,
            custom,
            phase,
            sched,
        } = &mut self.nodes[i].kind
        else {
            unreachable!()
        };
        let mut out = vec![0.0f32; QUANTUM];
        let Some((s, e)) = sched.frames(f64::from(sr)) else {
            return vec![out];
        };
        let shape = *shape;
        let table = match (shape, custom.as_ref()) {
            (Shape::Custom, Some(c)) => c.clone(),
            _ => wave
                .get_or_insert_with(|| std::rc::Rc::new(PeriodicWave::basic(sr, shape)))
                .clone(),
        };
        let size = table.size as f64;
        let nyquist = sr / 2.0;
        for (k, v) in out.iter_mut().enumerate() {
            let f = frame + k as u64;
            if f < s || f >= e {
                continue;
            }
            let fr = freq[k.min(freq.len() - 1)];
            let d = det[k.min(det.len() - 1)];
            let hz = (fr * 2f32.powf(d / 1200.0)).clamp(-nyquist, nyquist);
            *v = table.sample(hz, *phase);
            let incr = f64::from(hz * table.rate_scale);
            *phase += incr;
            *phase -= (*phase / size).floor() * size;
        }
        vec![out]
    }

    fn buffer_source(&mut self, i: usize) -> Vec<Vec<f32>> {
        let sr = f64::from(self.sample_rate);
        let frame = self.frame;
        let rate = self.param(i, 0)[0];
        let det = self.param(i, 1)[0];
        let Kind::BufferSource {
            buffer,
            buffer_rate,
            pos,
            looping,
            loop_start,
            loop_end,
            offset,
            sched,
        } = &mut self.nodes[i].kind
        else {
            unreachable!()
        };
        let Some(buf) = buffer.clone() else {
            return Self::silence(1);
        };
        let ch = buf.len().max(1);
        let mut out = Self::silence(ch);
        let Some((s, e)) = sched.frames(sr) else {
            return out;
        };
        let len = buf.first().map_or(0, Vec::len);
        if len == 0 {
            return out;
        }
        let step =
            f64::from(rate) * 2f64.powf(f64::from(det) / 1200.0) * f64::from(*buffer_rate) / sr;
        let bsr = f64::from(*buffer_rate);
        let (ls, le) = if *looping && *loop_end > *loop_start && *loop_start >= 0.0 {
            (
                (*loop_start * bsr).min(len as f64),
                (*loop_end * bsr).min(len as f64),
            )
        } else {
            (0.0, len as f64)
        };
        for k in 0..QUANTUM {
            let f = frame + k as u64;
            if f < s || f >= e {
                continue;
            }
            if f == s {
                *pos = *offset * bsr;
            }
            if *looping {
                if *pos >= le {
                    *pos = ls + (*pos - le) % (le - ls).max(1.0);
                }
            } else if *pos >= len as f64 || *pos < 0.0 {
                break;
            }
            let i0 = pos.floor() as usize;
            let fr = (*pos - i0 as f64) as f32;
            for (c, o) in out.iter_mut().enumerate() {
                let d = &buf[c];
                let a = d.get(i0).copied().unwrap_or(0.0);
                let b = d.get(i0 + 1).copied().unwrap_or(0.0);
                o[k] = a + (b - a) * fr;
            }
            *pos += step;
        }
        out
    }

    /// Renders one quantum and returns the destination's channels.
    pub fn render_quantum(&mut self) -> Vec<Vec<f32>> {
        // Analysers and other sinks not connected to the destination still
        // pull their inputs (Blink's automatic pull nodes).
        for k in 1..self.nodes.len() {
            if matches!(self.nodes[k].kind, Kind::Analyser(_)) {
                self.render(k);
            }
        }
        self.render(0);
        let out = self.nodes[0].out.clone();
        self.frame += QUANTUM as u64;
        out
    }

    /// Renders `frames` frames (`OfflineAudioContext.startRendering`).
    pub fn render_frames(&mut self, frames: usize) -> Vec<Vec<f32>> {
        let ch = self.nodes[0].channel_count.max(1);
        let mut out = vec![Vec::with_capacity(frames); ch];
        while out[0].len() < frames {
            let q = self.render_quantum();
            let take = (frames - out[0].len()).min(QUANTUM);
            for (c, o) in out.iter_mut().enumerate() {
                o.extend_from_slice(&q.get(c).unwrap_or(&q[0])[..take]);
            }
        }
        out
    }

    /// Sources whose stop frame has passed and that have not reported
    /// `ended` yet; marks them reported.
    pub fn take_ended(&mut self) -> Vec<usize> {
        let sr = f64::from(self.sample_rate);
        let now = self.frame;
        let mut out = Vec::new();
        for (i, n) in self.nodes.iter_mut().enumerate() {
            let sched = match &mut n.kind {
                Kind::Oscillator { sched, .. }
                | Kind::Constant { sched }
                | Kind::BufferSource { sched, .. } => sched,
                _ => continue,
            };
            if let Some((_, e)) = sched.frames(sr)
                && e != u64::MAX
                && now >= e
                && !sched.ended
            {
                sched.ended = true;
                out.push(i);
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp_graph(seed: Option<[u8; 16]>) -> f64 {
        let sr = 44100.0;
        let mut g = Graph::new(sr, 1);
        let osc = g.add(Node::new(
            Kind::Oscillator {
                shape: Shape::Triangle,
                wave: None,
                custom: None,
                phase: 0.0,
                sched: Schedule {
                    start: Some(0.0),
                    ..Default::default()
                },
            },
            vec![
                Param {
                    value: 10000.0,
                    ..Param::new(440.0, -22050.0, 22050.0, true)
                },
                Param::new(0.0, -153600.0, 153600.0, true),
            ],
            2,
            CountMode::Max,
        ));
        let mut params = vec![
            Param::new(-24.0, -100.0, 0.0, false),
            Param::new(30.0, 0.0, 40.0, false),
            Param::new(12.0, 1.0, 20.0, false),
            Param::new(0.003, 0.0, 1.0, false),
            Param::new(0.25, 0.0, 1.0, false),
        ];
        for (p, v) in params.iter_mut().zip([-50.0, 40.0, 12.0, 0.0, 0.25]) {
            p.value = v;
        }
        let comp = g.add(Node::new(
            Kind::Compressor {
                kernel: Box::new(Compressor::new(sr, 2)),
                reduction: 0.0,
            },
            params,
            2,
            CountMode::ClampedMax,
        ));
        g.nodes[comp].inputs.push((osc, 0, 0));
        g.nodes[0].inputs.push((comp, 0, 0));
        let mut out = g.render_frames(5000);
        if let Some(s) = seed {
            apply_noise(s, 0, &mut out[0]);
        }
        out[0][4500..5000]
            .iter()
            .map(|v| f64::from(v.abs()))
            .sum::<f64>()
    }

    #[test]
    fn classic_audio_fingerprint_is_chrome_like() {
        let s = fp_graph(None);
        // Chrome 153 on x86-64 Linux: 124.04347776696522.
        assert!((s - 124.043_478).abs() < 0.001, "sum {s}");
        let a = fp_graph(Some([1; 16]));
        let b = fp_graph(Some([1; 16]));
        let c = fp_graph(Some([2; 16]));
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert!((a - s).abs() < 0.01);
    }

    #[test]
    fn triangle_oscillator_matches_chrome_samples() {
        let w = PeriodicWave::basic(44100.0, Shape::Triangle);
        let incr = f64::from(10000.0 * w.rate_scale);
        let got: Vec<f32> = (0..4).map(|k| w.sample(10000.0, k as f64 * incr)).collect();
        let want = [0.0, 0.802_099_8, 0.233_441_9, -0.734_159_2];
        for (g, w) in got.iter().zip(want) {
            assert!((g - w).abs() < 1e-4, "{got:?}");
        }
    }

    #[test]
    fn param_timeline() {
        let mut p = Param::new(1.0, 0.0, 10.0, true);
        p.insert(Event::Set { t: 0.0, v: 0.0 });
        p.insert(Event::Linear { t: 1.0, v: 1.0 });
        assert!((p.at(0.5) - 0.5).abs() < 1e-6);
        assert_eq!(p.at(2.0), 1.0);
    }

    #[test]
    fn fft_roundtrip() {
        let mut re: Vec<f64> = (0..8).map(|i| i as f64).collect();
        let mut im = vec![0.0; 8];
        fft(&mut re, &mut im, false);
        fft(&mut re, &mut im, true);
        for (i, v) in re.iter().enumerate() {
            assert!((v / 8.0 - i as f64).abs() < 1e-9);
        }
    }
}
