//! The Canvas 2D rendering context on a tiny-skia pixmap.
//!
//! [`Canvas2d`] holds the bitmap (allocated on first draw), the drawing
//! state stack and the current path. Coordinates are `f64` like the Web
//! API; calls with non-finite arguments are ignored, as the spec says.
//! Argument validation that throws (negative radii, bad repetition, ...)
//! is the caller's job.
//!
//! Compositing: bounded operators draw straight into the bitmap. Shadows
//! and the operators that also clear outside the shape (`source-in`,
//! `source-out`, `destination-in`, `destination-atop`, `copy`) render
//! the shape into a transparent layer first, then composite the
//! (blurred) shadow and the layer over the whole bitmap. The clip is a
//! coverage mask.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use lb_css::Color;
use lb_layout::TextFont;
use lb_persona::PersonaProfile;
use tiny_skia::{
    BlendMode, FillRule, FilterQuality, GradientStop, LineCap, LineJoin, LinearGradient, Mask,
    Paint, Path, Pattern, Pixmap, PixmapPaint, Point, RadialGradient, Shader, SpreadMode, Stroke,
    StrokeDash, SweepGradient, Transform,
};

use crate::color::to_skia;
use crate::encode::{self, Bitmap};
use crate::font::{CanvasFont, FontSet, TextAlign, TextBaseline, TextMetrics, TextStyle};
use crate::noise::CanvasNoise;
use crate::path::{PathData, point_in};

/// Largest canvas Chrome allocates (area and side).
pub const MAX_AREA: u64 = 268_435_456;
pub const MAX_SIDE: u32 = 32_767;

/// Per-persona paint resources, shared by every canvas of a window.
#[derive(Debug)]
pub struct PaintEnv {
    pub fonts: FontSet,
    /// Readback noise; `None` without a persona seed.
    pub noise: Option<CanvasNoise>,
}

impl PaintEnv {
    pub fn new(profile: Option<&PersonaProfile>) -> PaintEnv {
        PaintEnv {
            fonts: FontSet::new(profile),
            noise: profile
                .and_then(|p| p.fingerprint_seeds.canvas_bytes())
                .map(CanvasNoise::new),
        }
    }
}

/// `CanvasGradient`.
#[derive(Debug)]
pub struct Gradient {
    kind: GradientKind,
    stops: RefCell<Vec<(f64, Color)>>,
}

#[derive(Clone, Copy, Debug)]
enum GradientKind {
    Linear([f64; 4]),
    Radial([f64; 6]),
    Conic([f64; 3]),
}

impl Gradient {
    pub fn linear(x0: f64, y0: f64, x1: f64, y1: f64) -> Gradient {
        Self::make(GradientKind::Linear([x0, y0, x1, y1]))
    }
    /// The caller rejects negative radii (`IndexSizeError`).
    pub fn radial(x0: f64, y0: f64, r0: f64, x1: f64, y1: f64, r1: f64) -> Gradient {
        Self::make(GradientKind::Radial([x0, y0, r0, x1, y1, r1]))
    }
    pub fn conic(start: f64, x: f64, y: f64) -> Gradient {
        Self::make(GradientKind::Conic([start, x, y]))
    }
    fn make(kind: GradientKind) -> Gradient {
        Gradient {
            kind,
            stops: RefCell::new(Vec::new()),
        }
    }
    /// `addColorStop`; the caller checks `0 <= offset <= 1`.
    pub fn add_color_stop(&self, offset: f64, color: Color) {
        let mut s = self.stops.borrow_mut();
        let at = s.iter().position(|(o, _)| *o > offset).unwrap_or(s.len());
        s.insert(at, (offset, color));
    }

    fn shader(&self, ts: Transform, alpha: f32) -> Option<Shader<'static>> {
        let stops: Vec<GradientStop> = self
            .stops
            .borrow()
            .iter()
            .map(|(o, c)| GradientStop::new(*o as f32, to_skia(*c, 1.0)))
            .collect();
        if stops.is_empty() {
            return None;
        }
        let f = |v: f64| v as f32;
        let mut sh = match self.kind {
            GradientKind::Linear([x0, y0, x1, y1]) => {
                if x0 == x1 && y0 == y1 {
                    return None;
                }
                LinearGradient::new(
                    Point::from_xy(f(x0), f(y0)),
                    Point::from_xy(f(x1), f(y1)),
                    stops,
                    SpreadMode::Pad,
                    ts,
                )?
            }
            GradientKind::Radial([x0, y0, r0, x1, y1, r1]) => {
                if x0 == x1 && y0 == y1 && r0 == r1 {
                    return None;
                }
                RadialGradient::new(
                    Point::from_xy(f(x0), f(y0)),
                    f(r0),
                    Point::from_xy(f(x1), f(y1)),
                    f(r1),
                    stops,
                    SpreadMode::Pad,
                    ts,
                )?
            }
            GradientKind::Conic([start, x, y]) => {
                let rot = rotate_at(start, x, y);
                SweepGradient::new(
                    Point::from_xy(f(x), f(y)),
                    0.0,
                    360.0,
                    stops,
                    SpreadMode::Pad,
                    ts.pre_concat(rot),
                )?
            }
        };
        sh.apply_opacity(alpha);
        Some(sh)
    }
}

fn rotate_at(rad: f64, x: f64, y: f64) -> Transform {
    let (s, c) = rad.sin_cos();
    let (s, c, x, y) = (s as f32, c as f32, x as f32, y as f32);
    Transform::from_row(c, s, -s, c, x - c * x + s * y, y - s * x - c * y)
}

/// `CanvasPattern` repetition.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Repetition {
    Repeat,
    RepeatX,
    RepeatY,
    NoRepeat,
}

impl Repetition {
    /// `""` and `"repeat"` repeat; `None` for an invalid value
    /// (`SyntaxError`).
    pub fn parse(s: &str) -> Option<Repetition> {
        Some(match s {
            "" | "repeat" => Repetition::Repeat,
            "repeat-x" => Repetition::RepeatX,
            "repeat-y" => Repetition::RepeatY,
            "no-repeat" => Repetition::NoRepeat,
            _ => return None,
        })
    }
}

/// `CanvasPattern`.
#[derive(Debug)]
pub struct CanvasPattern {
    pixmap: Rc<Pixmap>,
    repeat: Repetition,
    transform: Cell<Transform>,
}

impl CanvasPattern {
    pub fn new(pixmap: Rc<Pixmap>, repeat: Repetition) -> CanvasPattern {
        CanvasPattern {
            pixmap,
            repeat,
            transform: Cell::new(Transform::identity()),
        }
    }
    /// `setTransform(matrix)` as `[a, b, c, d, e, f]`.
    pub fn set_transform(&self, m: [f64; 6]) {
        if m.iter().all(|v| v.is_finite()) {
            self.transform.set(to_ts(m));
        }
    }
}

/// `fillStyle` / `strokeStyle`.
#[derive(Clone, Debug)]
pub enum Style {
    Color(Color),
    Gradient(Rc<Gradient>),
    Pattern(Rc<CanvasPattern>),
}

fn to_ts(m: [f64; 6]) -> Transform {
    let f = |v: f64| v as f32;
    Transform::from_row(f(m[0]), f(m[1]), f(m[2]), f(m[3]), f(m[4]), f(m[5]))
}

/// `globalCompositeOperation` names and their blend modes.
const COMPOSITE: &[(&str, BlendMode)] = &[
    ("source-over", BlendMode::SourceOver),
    ("source-in", BlendMode::SourceIn),
    ("source-out", BlendMode::SourceOut),
    ("source-atop", BlendMode::SourceAtop),
    ("destination-over", BlendMode::DestinationOver),
    ("destination-in", BlendMode::DestinationIn),
    ("destination-out", BlendMode::DestinationOut),
    ("destination-atop", BlendMode::DestinationAtop),
    ("lighter", BlendMode::Plus),
    ("copy", BlendMode::Source),
    ("xor", BlendMode::Xor),
    ("multiply", BlendMode::Multiply),
    ("screen", BlendMode::Screen),
    ("overlay", BlendMode::Overlay),
    ("darken", BlendMode::Darken),
    ("lighten", BlendMode::Lighten),
    ("color-dodge", BlendMode::ColorDodge),
    ("color-burn", BlendMode::ColorBurn),
    ("hard-light", BlendMode::HardLight),
    ("soft-light", BlendMode::SoftLight),
    ("difference", BlendMode::Difference),
    ("exclusion", BlendMode::Exclusion),
    ("hue", BlendMode::Hue),
    ("saturation", BlendMode::Saturation),
    ("color", BlendMode::Color),
    ("luminosity", BlendMode::Luminosity),
];

/// One entry of the drawing state stack.
#[derive(Clone, Debug)]
struct State {
    transform: Transform,
    fill: Style,
    stroke: Style,
    line_width: f64,
    line_cap: LineCap,
    line_join: LineJoin,
    miter_limit: f64,
    dash: Vec<f64>,
    dash_offset: f64,
    global_alpha: f64,
    composite: usize,
    shadow_x: f64,
    shadow_y: f64,
    shadow_blur: f64,
    shadow_color: Color,
    font: CanvasFont,
    font_css: String,
    resolved: Option<TextFont>,
    align: TextAlign,
    baseline: TextBaseline,
    rtl_dir: Option<bool>,
    letter_spacing: f32,
    word_spacing: f32,
    smoothing: bool,
    smoothing_quality: &'static str,
    clip: Option<Rc<Mask>>,
    /// Stored string attributes without rendering effect yet.
    filter: String,
    font_kerning: &'static str,
    font_stretch: &'static str,
    font_variant_caps: &'static str,
    text_rendering: &'static str,
}

impl Default for State {
    fn default() -> Self {
        let font = CanvasFont::default();
        State {
            transform: Transform::identity(),
            fill: Style::Color(Color::BLACK),
            stroke: Style::Color(Color::BLACK),
            line_width: 1.0,
            line_cap: LineCap::Butt,
            line_join: LineJoin::Miter,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_offset: 0.0,
            global_alpha: 1.0,
            composite: 0,
            shadow_x: 0.0,
            shadow_y: 0.0,
            shadow_blur: 0.0,
            shadow_color: Color::TRANSPARENT,
            font_css: font.to_css(),
            font,
            resolved: None,
            align: TextAlign::Start,
            baseline: TextBaseline::Alphabetic,
            rtl_dir: None,
            letter_spacing: 0.0,
            word_spacing: 0.0,
            smoothing: true,
            smoothing_quality: "low",
            clip: None,
            filter: "none".into(),
            font_kerning: "auto",
            font_stretch: "normal",
            font_variant_caps: "normal",
            text_rendering: "auto",
        }
    }
}

enum Shape {
    /// Device-space path; the paint's shader is in device space.
    Fill(Path, FillRule),
    /// User-space path stroked under the current transform; the shader is
    /// in user space.
    Stroke(Path, Stroke),
}

/// A 2D context and its bitmap.
pub struct Canvas2d {
    env: Rc<PaintEnv>,
    width: u32,
    height: u32,
    pixmap: Option<Pixmap>,
    state: State,
    stack: Vec<State>,
    path: PathData,
    opaque: bool,
    /// Drew a cross-origin image: readback throws `SecurityError`.
    pub tainted: bool,
}

impl std::fmt::Debug for Canvas2d {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Canvas2d")
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

fn finite(v: &[f64]) -> bool {
    v.iter().all(|x| x.is_finite())
}

impl Canvas2d {
    /// A transparent `width` x `height` canvas (`alpha: false` makes it
    /// opaque black).
    pub fn new(env: Rc<PaintEnv>, width: u32, height: u32, alpha: bool) -> Canvas2d {
        Canvas2d {
            env,
            width,
            height,
            pixmap: None,
            state: State::default(),
            stack: Vec::new(),
            path: PathData::new(),
            opaque: !alpha,
            tainted: false,
        }
    }

    pub fn env(&self) -> &Rc<PaintEnv> {
        &self.env
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    /// `getContextAttributes().alpha`.
    pub fn alpha(&self) -> bool {
        !self.opaque
    }

    fn has_backing(&self) -> bool {
        self.width > 0
            && self.height > 0
            && self.width <= MAX_SIDE
            && self.height <= MAX_SIDE
            && u64::from(self.width) * u64::from(self.height) <= MAX_AREA
    }

    fn ensure_pixmap(&mut self) -> Option<&mut Pixmap> {
        if self.pixmap.is_none() && self.has_backing() {
            let mut p = Pixmap::new(self.width, self.height)?;
            if self.opaque {
                p.fill(tiny_skia::Color::BLACK);
            }
            self.pixmap = Some(p);
        }
        self.pixmap.as_mut()
    }

    /// New size (canvas `width`/`height` set): clears the bitmap and
    /// resets the drawing state and path.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.reset();
    }

    /// `reset()`.
    pub fn reset(&mut self) {
        self.pixmap = None;
        self.state = State::default();
        self.stack.clear();
        self.path.clear();
    }

    // ----- state -----

    pub fn save(&mut self) {
        self.stack.push(self.state.clone());
    }

    pub fn restore(&mut self) {
        if let Some(s) = self.stack.pop() {
            self.state = s;
        }
    }

    pub fn global_alpha(&self) -> f64 {
        self.state.global_alpha
    }
    pub fn set_global_alpha(&mut self, a: f64) {
        if a.is_finite() && (0.0..=1.0).contains(&a) {
            self.state.global_alpha = a;
        }
    }

    pub fn composite(&self) -> &'static str {
        COMPOSITE[self.state.composite].0
    }
    pub fn set_composite(&mut self, op: &str) {
        if let Some(i) = COMPOSITE.iter().position(|(n, _)| *n == op) {
            self.state.composite = i;
        }
    }

    pub fn line_width(&self) -> f64 {
        self.state.line_width
    }
    pub fn set_line_width(&mut self, w: f64) {
        if w.is_finite() && w > 0.0 {
            self.state.line_width = w;
        }
    }

    pub fn line_cap(&self) -> &'static str {
        match self.state.line_cap {
            LineCap::Butt => "butt",
            LineCap::Round => "round",
            LineCap::Square => "square",
        }
    }
    pub fn set_line_cap(&mut self, v: &str) {
        self.state.line_cap = match v {
            "butt" => LineCap::Butt,
            "round" => LineCap::Round,
            "square" => LineCap::Square,
            _ => return,
        }
    }

    pub fn line_join(&self) -> &'static str {
        match self.state.line_join {
            LineJoin::Round => "round",
            LineJoin::Bevel => "bevel",
            _ => "miter",
        }
    }
    pub fn set_line_join(&mut self, v: &str) {
        self.state.line_join = match v {
            "miter" => LineJoin::Miter,
            "round" => LineJoin::Round,
            "bevel" => LineJoin::Bevel,
            _ => return,
        }
    }

    pub fn miter_limit(&self) -> f64 {
        self.state.miter_limit
    }
    pub fn set_miter_limit(&mut self, v: f64) {
        if v.is_finite() && v > 0.0 {
            self.state.miter_limit = v;
        }
    }

    /// `setLineDash`: ignored if any value is negative or non-finite; an
    /// odd list is repeated.
    pub fn set_line_dash(&mut self, mut d: Vec<f64>) {
        if d.iter().any(|v| !v.is_finite() || *v < 0.0) {
            return;
        }
        if d.len() % 2 == 1 {
            d.extend_from_within(..);
        }
        self.state.dash = d;
    }
    pub fn line_dash(&self) -> &[f64] {
        &self.state.dash
    }
    pub fn line_dash_offset(&self) -> f64 {
        self.state.dash_offset
    }
    pub fn set_line_dash_offset(&mut self, v: f64) {
        if v.is_finite() {
            self.state.dash_offset = v;
        }
    }

    pub fn fill_style(&self) -> &Style {
        &self.state.fill
    }
    pub fn set_fill_style(&mut self, s: Style) {
        self.state.fill = s;
    }
    pub fn stroke_style(&self) -> &Style {
        &self.state.stroke
    }
    pub fn set_stroke_style(&mut self, s: Style) {
        self.state.stroke = s;
    }

    pub fn shadow_offset(&self) -> (f64, f64) {
        (self.state.shadow_x, self.state.shadow_y)
    }
    pub fn set_shadow_offset_x(&mut self, v: f64) {
        if v.is_finite() {
            self.state.shadow_x = v;
        }
    }
    pub fn set_shadow_offset_y(&mut self, v: f64) {
        if v.is_finite() {
            self.state.shadow_y = v;
        }
    }
    pub fn shadow_blur(&self) -> f64 {
        self.state.shadow_blur
    }
    pub fn set_shadow_blur(&mut self, v: f64) {
        if v.is_finite() && v >= 0.0 {
            self.state.shadow_blur = v;
        }
    }
    pub fn shadow_color(&self) -> Color {
        self.state.shadow_color
    }
    pub fn set_shadow_color(&mut self, c: Color) {
        self.state.shadow_color = c;
    }

    /// `font` getter (Chrome's serialization).
    pub fn font(&self) -> &str {
        &self.state.font_css
    }
    /// `font` setter; invalid values are ignored.
    pub fn set_font(&mut self, v: &str) {
        if let Some(f) = CanvasFont::parse_with(v, Some(self.env.fonts.defaults())) {
            self.state.font_css = f.to_css();
            self.state.font = f;
            self.state.resolved = None;
        }
    }

    pub fn text_align(&self) -> TextAlign {
        self.state.align
    }
    pub fn set_text_align(&mut self, a: TextAlign) {
        self.state.align = a;
    }
    pub fn text_baseline(&self) -> TextBaseline {
        self.state.baseline
    }
    pub fn set_text_baseline(&mut self, b: TextBaseline) {
        self.state.baseline = b;
    }
    /// `direction`: `ltr`, `rtl` or `inherit`.
    pub fn direction(&self) -> &'static str {
        match self.state.rtl_dir {
            None => "inherit",
            Some(false) => "ltr",
            Some(true) => "rtl",
        }
    }
    pub fn set_direction(&mut self, v: &str) {
        self.state.rtl_dir = match v {
            "inherit" => None,
            "ltr" => Some(false),
            "rtl" => Some(true),
            _ => return,
        }
    }

    /// `letterSpacing` / `wordSpacing` in px (the caller parses CSS).
    pub fn spacing(&self) -> (f32, f32) {
        (self.state.letter_spacing, self.state.word_spacing)
    }
    pub fn set_spacing(&mut self, letter: Option<f32>, word: Option<f32>) {
        if let Some(l) = letter {
            self.state.letter_spacing = l;
        }
        if let Some(w) = word {
            self.state.word_spacing = w;
        }
        self.state.resolved = None;
    }

    pub fn image_smoothing(&self) -> (bool, &'static str) {
        (self.state.smoothing, self.state.smoothing_quality)
    }
    pub fn set_image_smoothing_enabled(&mut self, on: bool) {
        self.state.smoothing = on;
    }
    pub fn set_image_smoothing_quality(&mut self, q: &str) {
        self.state.smoothing_quality = match q {
            "low" => "low",
            "medium" => "medium",
            "high" => "high",
            _ => return,
        }
    }

    /// `filter` (stored; filters are not rendered).
    pub fn filter(&self) -> &str {
        &self.state.filter
    }
    pub fn set_filter(&mut self, v: &str) {
        self.state.filter = v.to_owned();
    }

    /// `fontKerning`, `fontStretch`, `fontVariantCaps`, `textRendering`.
    pub fn text_flags(&self) -> [&'static str; 4] {
        [
            self.state.font_kerning,
            self.state.font_stretch,
            self.state.font_variant_caps,
            self.state.text_rendering,
        ]
    }
    /// Sets one of [`Canvas2d::text_flags`] (index as there) if `v` is
    /// valid for it.
    pub fn set_text_flag(&mut self, which: usize, v: &str) {
        const KERNING: &[&str] = &["auto", "normal", "none"];
        const STRETCH: &[&str] = &[
            "ultra-condensed",
            "extra-condensed",
            "condensed",
            "semi-condensed",
            "normal",
            "semi-expanded",
            "expanded",
            "extra-expanded",
            "ultra-expanded",
        ];
        const CAPS: &[&str] = &[
            "normal",
            "small-caps",
            "all-small-caps",
            "petite-caps",
            "all-petite-caps",
            "unicase",
            "titling-caps",
        ];
        const RENDERING: &[&str] = &[
            "auto",
            "optimizeSpeed",
            "optimizeLegibility",
            "geometricPrecision",
        ];
        let (list, slot) = match which {
            0 => (KERNING, &mut self.state.font_kerning),
            1 => (STRETCH, &mut self.state.font_stretch),
            2 => (CAPS, &mut self.state.font_variant_caps),
            3 => (RENDERING, &mut self.state.text_rendering),
            _ => return,
        };
        if let Some(s) = list.iter().find(|s| **s == v) {
            *slot = s;
        }
    }

    // ----- transforms -----

    /// Current transform as `[a, b, c, d, e, f]`.
    pub fn get_transform(&self) -> [f64; 6] {
        let t = &self.state.transform;
        [t.sx, t.ky, t.kx, t.sy, t.tx, t.ty].map(f64::from)
    }

    /// `transform(a, b, c, d, e, f)`: multiplies the current transform.
    pub fn transform(&mut self, m: [f64; 6]) {
        if finite(&m) {
            self.state.transform = self.state.transform.pre_concat(to_ts(m));
        }
    }

    pub fn set_transform(&mut self, m: [f64; 6]) {
        if finite(&m) {
            self.state.transform = to_ts(m);
        }
    }

    pub fn reset_transform(&mut self) {
        self.state.transform = Transform::identity();
    }

    pub fn translate(&mut self, x: f64, y: f64) {
        self.transform([1.0, 0.0, 0.0, 1.0, x, y]);
    }

    pub fn scale(&mut self, x: f64, y: f64) {
        self.transform([x, 0.0, 0.0, y, 0.0, 0.0]);
    }

    pub fn rotate(&mut self, rad: f64) {
        if rad.is_finite() {
            let (s, c) = rad.sin_cos();
            self.transform([c, s, -s, c, 0.0, 0.0]);
        }
    }

    // ----- paths -----

    /// Runs a path operation on the current path with the current
    /// transform (points are stored in device space).
    pub fn path_op(&mut self, f: impl FnOnce(&mut PathData, &Transform)) {
        let ts = self.state.transform;
        f(&mut self.path, &ts);
    }

    pub fn begin_path(&mut self) {
        self.path.clear();
    }

    /// The device-space path to use: the current path or `path` (a
    /// `Path2D`, user space) mapped through the current transform.
    fn device_path(&self, path: Option<&PathData>) -> PathData {
        match path {
            Some(p) => p.transformed(&self.state.transform),
            None => self.path.clone(),
        }
    }

    /// `fill([path], fillRule)`.
    pub fn fill(&mut self, path: Option<&PathData>, rule: FillRule) {
        let dev = self.device_path(path);
        if let Some(p) = dev.to_skia() {
            let style = self.state.fill.clone();
            self.render_style(Shape::Fill(p, rule), &style);
        }
    }

    /// `stroke([path])`.
    pub fn stroke(&mut self, path: Option<&PathData>) {
        let user = match path {
            Some(p) => Some(p.clone()),
            None => self
                .state
                .transform
                .invert()
                .map(|inv| self.path.transformed(&inv)),
        };
        if let Some(p) = user.and_then(|u| u.to_skia()) {
            self.stroke_user(p);
        }
    }

    fn stroke_params(&self) -> Stroke {
        let s = &self.state;
        Stroke {
            width: s.line_width as f32,
            miter_limit: s.miter_limit as f32,
            line_cap: s.line_cap,
            line_join: s.line_join,
            dash: if s.dash.is_empty() {
                None
            } else {
                StrokeDash::new(
                    s.dash.iter().map(|v| *v as f32).collect(),
                    s.dash_offset as f32,
                )
            },
        }
    }

    fn stroke_user(&mut self, p: Path) {
        if self.state.transform.invert().is_none() {
            return;
        }
        let stroke = self.stroke_params();
        let style = self.state.stroke.clone();
        self.render_style(Shape::Stroke(p, stroke), &style);
    }

    /// `clip([path], fillRule)`.
    pub fn clip(&mut self, path: Option<&PathData>, rule: FillRule) {
        if !self.has_backing() {
            return;
        }
        let dev = self.device_path(path).to_skia();
        let mask = match (&self.state.clip, dev) {
            (_, None) => Mask::new(self.width, self.height),
            (None, Some(p)) => Mask::new(self.width, self.height).map(|mut m| {
                m.fill_path(&p, rule, true, Transform::identity());
                m
            }),
            (Some(old), Some(p)) => {
                let mut m = (**old).clone();
                m.intersect_path(&p, rule, true, Transform::identity());
                Some(m)
            }
        };
        self.state.clip = mask.map(Rc::new);
    }

    /// `isPointInPath`: (`x`, `y`) in canvas pixels, untransformed.
    pub fn is_point_in_path(
        &self,
        path: Option<&PathData>,
        x: f64,
        y: f64,
        rule: FillRule,
    ) -> bool {
        self.device_path(path).contains(x, y, rule)
    }

    /// `isPointInStroke`.
    pub fn is_point_in_stroke(&self, path: Option<&PathData>, x: f64, y: f64) -> bool {
        let ts = self.state.transform;
        let user = match path {
            Some(p) => Some(p.clone()),
            None => ts.invert().map(|inv| self.path.transformed(&inv)),
        };
        user.and_then(|u| u.to_skia())
            .and_then(|p| p.stroke(&self.stroke_params(), 1.0))
            .and_then(|s| s.transform(ts))
            .is_some_and(|s| point_in(&s, x, y, FillRule::Winding))
    }

    // ----- rectangles -----

    fn rect_path(&self, x: f64, y: f64, w: f64, h: f64) -> PathData {
        let mut p = PathData::new();
        p.rect(&self.state.transform, x, y, w, h);
        p
    }

    pub fn fill_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if !finite(&[x, y, w, h]) || w == 0.0 || h == 0.0 {
            return;
        }
        if let Some(p) = self.rect_path(x, y, w, h).to_skia() {
            let style = self.state.fill.clone();
            self.render_style(Shape::Fill(p, FillRule::Winding), &style);
        }
    }

    pub fn stroke_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if !finite(&[x, y, w, h]) {
            return;
        }
        let mut p = PathData::new();
        p.rect(&Transform::identity(), x, y, w, h);
        if let Some(p) = p.to_skia() {
            self.stroke_user(p);
        }
    }

    pub fn clear_rect(&mut self, x: f64, y: f64, w: f64, h: f64) {
        if !finite(&[x, y, w, h]) || w == 0.0 || h == 0.0 || self.pixmap.is_none() {
            return;
        }
        let Some(p) = self.rect_path(x, y, w, h).to_skia() else {
            return;
        };
        let clear = if self.opaque {
            tiny_skia::Color::BLACK
        } else {
            tiny_skia::Color::TRANSPARENT
        };
        let mut paint = Paint {
            blend_mode: BlendMode::Source,
            ..Paint::default()
        };
        paint.set_color(clear);
        let clip = self.state.clip.clone();
        if let Some(pm) = self.pixmap.as_mut() {
            pm.fill_path(
                &p,
                &paint,
                FillRule::Winding,
                Transform::identity(),
                clip.as_deref(),
            );
        }
    }

    // ----- text -----

    fn text_style(&mut self) -> TextStyle {
        if self.state.resolved.is_none() {
            let (l, w) = (self.state.letter_spacing, self.state.word_spacing);
            self.state.resolved = Some(self.env.fonts.resolve(&self.state.font, l, w));
        }
        TextStyle {
            font: self.state.resolved.clone().expect("resolved font"),
            align: self.state.align,
            baseline: self.state.baseline,
            rtl: self.state.rtl_dir == Some(true),
        }
    }

    /// `measureText`.
    pub fn measure_text(&mut self, text: &str) -> TextMetrics {
        let st = self.text_style();
        self.env.fonts.measure(&st, text)
    }

    /// `fillText`; a `max_width` that is not positive draws nothing.
    pub fn fill_text(&mut self, text: &str, x: f64, y: f64, max_width: Option<f64>) {
        if !finite(&[x, y]) || max_width.is_some_and(|m| !(m.is_finite() && m > 0.0)) {
            return;
        }
        let st = self.text_style();
        let ts = self.state.transform;
        let path = self.env.fonts.text_path(&st, text, x, y, max_width, &ts);
        if let Some(p) = path.to_skia() {
            let style = self.state.fill.clone();
            self.render_style(Shape::Fill(p, FillRule::Winding), &style);
        }
        // Emoji keep their own colours (Chrome's colour emoji font).
        for (path, color) in self.env.fonts.emoji_layers(&st, text, x, y, max_width, &ts) {
            if let Some(p) = path.to_skia() {
                self.render_style(Shape::Fill(p, FillRule::Winding), &Style::Color(color));
            }
        }
    }

    /// `strokeText`.
    pub fn stroke_text(&mut self, text: &str, x: f64, y: f64, max_width: Option<f64>) {
        if !finite(&[x, y]) || max_width.is_some_and(|m| !(m.is_finite() && m > 0.0)) {
            return;
        }
        let st = self.text_style();
        let path = self
            .env
            .fonts
            .text_path(&st, text, x, y, max_width, &Transform::identity());
        if let Some(p) = path.to_skia() {
            self.stroke_user(p);
        }
    }

    // ----- images -----

    fn quality(&self) -> FilterQuality {
        match (self.state.smoothing, self.state.smoothing_quality) {
            (false, _) => FilterQuality::Nearest,
            (true, "high") => FilterQuality::Bicubic,
            _ => FilterQuality::Bilinear,
        }
    }

    /// `drawImage(src, sx, sy, sw, sh, dx, dy, dw, dh)` with `src`
    /// premultiplied. The source rectangle is clipped to the image and the
    /// destination scaled to match.
    #[allow(clippy::too_many_arguments)]
    pub fn draw_image(
        &mut self,
        src: &Pixmap,
        sx: f64,
        sy: f64,
        sw: f64,
        sh: f64,
        dx: f64,
        dy: f64,
        dw: f64,
        dh: f64,
    ) {
        if !finite(&[sx, sy, sw, sh, dx, dy, dw, dh]) {
            return;
        }
        let norm = |p: f64, l: f64| if l < 0.0 { (p + l, -l) } else { (p, l) };
        let ((mut sx, mut sw), (mut sy, mut sh)) = (norm(sx, sw), norm(sy, sh));
        let ((mut dx, mut dw), (mut dy, mut dh)) = (norm(dx, dw), norm(dy, dh));
        if sw == 0.0 || sh == 0.0 || dw == 0.0 || dh == 0.0 {
            return;
        }
        let (iw, ih) = (f64::from(src.width()), f64::from(src.height()));
        let (kx, ky) = (dw / sw, dh / sh);
        if sx < 0.0 {
            dx -= sx * kx;
            dw += sx * kx;
            sw += sx;
            sx = 0.0;
        }
        if sy < 0.0 {
            dy -= sy * ky;
            dh += sy * ky;
            sh += sy;
            sy = 0.0;
        }
        if sx + sw > iw {
            dw -= (sx + sw - iw) * kx;
            sw = iw - sx;
        }
        if sy + sh > ih {
            dh -= (sy + sh - ih) * ky;
            sh = ih - sy;
        }
        if sw <= 0.0 || sh <= 0.0 || dw <= 0.0 || dh <= 0.0 {
            return;
        }
        let img_ts = Transform::from_row(
            (dw / sw) as f32,
            0.0,
            0.0,
            (dh / sh) as f32,
            (dx - sx * dw / sw) as f32,
            (dy - sy * dh / sh) as f32,
        );
        let Some(rect) = self.rect_path(dx, dy, dw, dh).to_skia() else {
            return;
        };
        let alpha = self.state.global_alpha as f32;
        let shader = Pattern::new(
            src.as_ref(),
            SpreadMode::Pad,
            self.quality(),
            alpha,
            self.state.transform.pre_concat(img_ts),
        );
        self.render(Shape::Fill(rect, FillRule::Winding), shader, None);
    }

    // ----- pixels -----

    /// The premultiplied bitmap, if anything was drawn.
    pub fn pixmap(&self) -> Option<&Pixmap> {
        self.pixmap.as_ref()
    }

    /// A premultiplied copy of the bitmap (for `drawImage(canvas)` and
    /// patterns). `None` for a zero-sized canvas.
    pub fn snapshot(&self) -> Option<Pixmap> {
        match &self.pixmap {
            Some(p) => Some(p.clone()),
            None if self.has_backing() => {
                let mut p = Pixmap::new(self.width, self.height)?;
                if self.opaque {
                    p.fill(tiny_skia::Color::BLACK);
                }
                Some(p)
            }
            None => None,
        }
    }

    /// Unpremultiplied pixels of a region; outside the canvas is
    /// transparent. `noise` adds the persona's readback noise.
    pub fn read(&self, sx: i64, sy: i64, sw: u32, sh: u32, noise: bool) -> Bitmap {
        let mut out = Bitmap::new(sw, sh);
        let (w, h) = (i64::from(self.width), i64::from(self.height));
        let Some(pm) = &self.pixmap else {
            if self.opaque {
                for (i, p) in out.data.as_chunks_mut::<4>().0.iter_mut().enumerate() {
                    let (x, y) = (
                        sx + (i as i64 % i64::from(sw)),
                        sy + (i as i64 / i64::from(sw)),
                    );
                    if x >= 0 && y >= 0 && x < w && y < h {
                        p[3] = 255;
                    }
                }
            }
            return out;
        };
        let data = pm.data();
        for oy in 0..i64::from(sh) {
            let y = sy + oy;
            if y < 0 || y >= h {
                continue;
            }
            for ox in 0..i64::from(sw) {
                let x = sx + ox;
                if x < 0 || x >= w {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                let o = ((oy * i64::from(sw) + ox) * 4) as usize;
                let a = data[i + 3];
                out.data[o + 3] = if self.opaque { 255 } else { a };
                if a == 255 || self.opaque {
                    out.data[o..o + 3].copy_from_slice(&data[i..i + 3]);
                } else if a > 0 {
                    for c in 0..3 {
                        let v = (u32::from(data[i + c]) * 255 + u32::from(a) / 2) / u32::from(a);
                        out.data[o + c] = v.min(255) as u8;
                    }
                }
            }
        }
        if noise && let Some(n) = &self.env.noise {
            n.apply(data, self.width, self.height, sx, sy, sw, &mut out.data);
        }
        out
    }

    /// `getImageData` (noised).
    pub fn get_image_data(&self, sx: i64, sy: i64, sw: u32, sh: u32) -> Bitmap {
        self.read(sx, sy, sw, sh, true)
    }

    /// The whole canvas, noised (what `toDataURL` / `toBlob` encode).
    pub fn export(&self) -> Bitmap {
        self.read(0, 0, self.width, self.height, true)
    }

    /// `toDataURL(type, quality)`.
    pub fn to_data_url(&self, mime: &str, quality: Option<f64>) -> String {
        if !self.has_backing() {
            return "data:,".into();
        }
        encode::data_url(&self.export(), mime, quality)
    }

    /// Encoded bytes for `toBlob` / `convertToBlob`, with the type used.
    pub fn encode(&self, mime: &str, quality: Option<f64>) -> (Vec<u8>, &'static str) {
        let ty = encode::output_type(mime);
        if !self.has_backing() {
            return (Vec::new(), ty);
        }
        (encode::encode(&self.export(), ty, quality), ty)
    }

    /// `putImageData(data, dx, dy, dirtyX, dirtyY, dirtyW, dirtyH)`:
    /// writes pixels as they are (no transform, alpha or compositing).
    #[allow(clippy::too_many_arguments)]
    pub fn put_image_data(
        &mut self,
        img: &Bitmap,
        dx: i64,
        dy: i64,
        dirty: Option<(i64, i64, i64, i64)>,
    ) {
        let (mut x0, mut y0, mut x1, mut y1) =
            (0i64, 0i64, i64::from(img.width), i64::from(img.height));
        if let Some((x, y, w, h)) = dirty {
            let (x, w) = if w < 0 { (x + w, -w) } else { (x, w) };
            let (y, h) = if h < 0 { (y + h, -h) } else { (y, h) };
            x0 = x0.max(x);
            y0 = y0.max(y);
            x1 = x1.min(x + w);
            y1 = y1.min(y + h);
        }
        let (cw, ch) = (i64::from(self.width), i64::from(self.height));
        x0 = x0.max(-dx);
        y0 = y0.max(-dy);
        x1 = x1.min(cw - dx);
        y1 = y1.min(ch - dy);
        if x0 >= x1 || y0 >= y1 {
            return;
        }
        let opaque = self.opaque;
        let Some(pm) = self.ensure_pixmap() else {
            return;
        };
        let data = pm.data_mut();
        for y in y0..y1 {
            for x in x0..x1 {
                let s = ((y * i64::from(img.width) + x) * 4) as usize;
                let d = (((y + dy) * cw + x + dx) * 4) as usize;
                let a = if opaque { 255 } else { img.data[s + 3] };
                let a16 = u16::from(a);
                for c in 0..3 {
                    data[d + c] = ((u16::from(img.data[s + c]) * a16 + 127) / 255) as u8;
                }
                data[d + 3] = a;
            }
        }
    }

    // ----- rendering -----

    fn render_style(&mut self, shape: Shape, style: &Style) {
        let alpha = self.state.global_alpha as f32;
        let ts = match shape {
            Shape::Fill(..) => self.state.transform,
            Shape::Stroke(..) => Transform::identity(),
        };
        match style {
            Style::Color(c) => {
                let c = to_skia(*c, alpha);
                if c.alpha() == 0.0 && self.blend() == BlendMode::SourceOver {
                    return;
                }
                self.render(shape, Shader::SolidColor(c), None);
            }
            Style::Gradient(g) => {
                if let Some(sh) = g.shader(ts, alpha) {
                    self.render(shape, sh, None);
                }
            }
            Style::Pattern(p) => {
                let p = p.clone();
                let pts = ts.pre_concat(p.transform.get());
                let spread = if p.repeat == Repetition::Repeat {
                    SpreadMode::Repeat
                } else {
                    SpreadMode::Pad
                };
                // Non-repeating axes: limit drawing to the image's strip.
                let strip = (p.repeat != Repetition::Repeat).then(|| {
                    const BIG: f32 = 1.0e5;
                    let (w, h) = (p.pixmap.width() as f32, p.pixmap.height() as f32);
                    let (x0, x1) = if p.repeat == Repetition::RepeatX {
                        (-BIG, BIG)
                    } else {
                        (0.0, w)
                    };
                    let (y0, y1) = if p.repeat == Repetition::RepeatY {
                        (-BIG, BIG)
                    } else {
                        (0.0, h)
                    };
                    let user_to_dev = match shape {
                        Shape::Fill(..) => pts,
                        Shape::Stroke(..) => self.state.transform.pre_concat(pts),
                    };
                    (tiny_skia::Rect::from_ltrb(x0, y0, x1, y1), user_to_dev)
                });
                let shader = Pattern::new(
                    p.pixmap.as_ref().as_ref(),
                    spread,
                    self.quality(),
                    alpha,
                    pts,
                );
                let extra = match strip {
                    Some((Some(r), ts)) => Some((PathBuilderRect(r).path(), ts)),
                    Some((None, _)) => return,
                    None => None,
                };
                self.render(shape, shader, extra);
            }
        }
    }

    fn blend(&self) -> BlendMode {
        COMPOSITE[self.state.composite].1
    }

    fn shadow_active(&self) -> bool {
        let s = &self.state;
        s.shadow_color.a > 0.0 && (s.shadow_blur > 0.0 || s.shadow_x != 0.0 || s.shadow_y != 0.0)
    }

    /// Draws `shape` with `shader`; `extra` limits coverage to a path
    /// (pattern strips).
    fn render(
        &mut self,
        shape: Shape,
        shader: Shader<'_>,
        extra: Option<(Option<Path>, Transform)>,
    ) {
        let (w, h) = (self.width, self.height);
        let mode = self.blend();
        let ctm = self.state.transform;
        let shadow = self.shadow_active();
        let unbounded = matches!(
            mode,
            BlendMode::SourceIn
                | BlendMode::SourceOut
                | BlendMode::DestinationIn
                | BlendMode::DestinationAtop
                | BlendMode::Source
        );
        let mask: Option<Rc<Mask>> = match extra {
            None => self.state.clip.clone(),
            Some((None, _)) => return,
            Some((Some(path), ts)) => {
                let m = match &self.state.clip {
                    Some(c) => {
                        let mut m = (**c).clone();
                        m.intersect_path(&path, FillRule::Winding, true, ts);
                        Some(m)
                    }
                    None => Mask::new(w, h).map(|mut m| {
                        m.fill_path(&path, FillRule::Winding, true, ts);
                        m
                    }),
                };
                m.map(Rc::new)
            }
        };
        let paint = Paint {
            shader,
            blend_mode: if shadow || unbounded {
                BlendMode::SourceOver
            } else {
                mode
            },
            anti_alias: true,
            ..Paint::default()
        };
        let draw = |pm: &mut Pixmap, m: Option<&Mask>| match &shape {
            Shape::Fill(p, rule) => pm.fill_path(p, &paint, *rule, Transform::identity(), m),
            Shape::Stroke(p, s) => pm.stroke_path(p, &paint, s, ctm, m),
        };
        let shadow_spec = shadow.then(|| {
            let s = &self.state;
            (s.shadow_color, s.shadow_blur, s.shadow_x, s.shadow_y)
        });
        let Some(pm) = self.ensure_pixmap() else {
            return;
        };
        if !shadow && !unbounded {
            draw(pm, mask.as_deref());
            return;
        }
        let Some(mut layer) = Pixmap::new(w, h) else {
            return;
        };
        // Shapes with an extra strip mask are clipped in the layer too.
        draw(&mut layer, None);
        let composite = |pm: &mut Pixmap, src: &Pixmap, dx: f64, dy: f64| {
            let paint = PixmapPaint {
                opacity: 1.0,
                blend_mode: mode,
                quality: if dx.fract() == 0.0 && dy.fract() == 0.0 {
                    FilterQuality::Nearest
                } else {
                    FilterQuality::Bilinear
                },
            };
            pm.draw_pixmap(
                0,
                0,
                src.as_ref(),
                &paint,
                Transform::from_translate(dx as f32, dy as f32),
                mask.as_deref(),
            );
        };
        if let Some((color, blur, ox, oy)) = shadow_spec
            && let Some(sh) = make_shadow(&layer, color, blur)
        {
            composite(pm, &sh, ox, oy);
        }
        composite(pm, &layer, 0.0, 0.0);
    }
}

/// A rectangle as a path (helper for pattern strips).
struct PathBuilderRect(tiny_skia::Rect);

impl PathBuilderRect {
    fn path(self) -> Option<Path> {
        Some(tiny_skia::PathBuilder::from_rect(self.0))
    }
}

/// Shadow of `layer`: its alpha, blurred (three box passes approximating
/// a Gaussian with sigma = blur / 2), tinted with `color`.
fn make_shadow(layer: &Pixmap, color: Color, blur: f64) -> Option<Pixmap> {
    let (w, h) = (layer.width() as usize, layer.height() as usize);
    let mut alpha: Vec<f32> = layer
        .data()
        .as_chunks::<4>()
        .0
        .iter()
        .map(|p| f32::from(p[3]))
        .collect();
    let sigma = blur / 2.0;
    if sigma > 0.0 {
        let d = (sigma * 3.0 * (2.0 * std::f64::consts::PI).sqrt() / 4.0 + 0.5).floor() as usize;
        let r = d / 2;
        if r > 0 {
            let mut tmp = vec![0f32; alpha.len()];
            for _ in 0..3 {
                box_blur(&alpha, &mut tmp, w, h, r, true);
                box_blur(&tmp, &mut alpha, w, h, r, false);
            }
        }
    }
    let mut out = Pixmap::new(w as u32, h as u32)?;
    let base = to_skia(color, 1.0);
    let (cr, cg, cb, ca) = (base.red(), base.green(), base.blue(), base.alpha());
    for (px, a) in out.data_mut().as_chunks_mut::<4>().0.iter_mut().zip(alpha) {
        let a = (a / 255.0).clamp(0.0, 1.0) * ca;
        if a <= 0.0 {
            continue;
        }
        px[0] = (cr * a * 255.0).round() as u8;
        px[1] = (cg * a * 255.0).round() as u8;
        px[2] = (cb * a * 255.0).round() as u8;
        px[3] = (a * 255.0).round() as u8;
    }
    Some(out)
}

fn box_blur(src: &[f32], dst: &mut [f32], w: usize, h: usize, r: usize, horizontal: bool) {
    let (n, lines, stride, step) = if horizontal {
        (w, h, w, 1)
    } else {
        (h, w, 1, w)
    };
    let k = 1.0 / (2 * r + 1) as f32;
    for line in 0..lines {
        let base = line * stride;
        let at = |i: isize| -> f32 {
            if i < 0 || i as usize >= n {
                0.0
            } else {
                src[base + i as usize * step]
            }
        };
        let mut acc: f32 = (-(r as isize)..=r as isize).map(at).sum();
        for i in 0..n {
            dst[base + i * step] = acc * k;
            acc += at(i as isize + r as isize + 1) - at(i as isize - r as isize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::color::parse_color;

    fn canvas(w: u32, h: u32) -> Canvas2d {
        Canvas2d::new(Rc::new(PaintEnv::new(None)), w, h, true)
    }

    fn px(c: &Canvas2d, x: i64, y: i64) -> [u8; 4] {
        c.read(x, y, 1, 1, false).data.try_into().unwrap()
    }

    #[test]
    fn fill_rect_and_clear() {
        let mut c = canvas(10, 10);
        assert_eq!(px(&c, 1, 1), [0, 0, 0, 0]);
        c.set_fill_style(Style::Color(parse_color("rgb(10,20,30)").unwrap()));
        c.fill_rect(0.0, 0.0, 4.0, 4.0);
        assert_eq!(px(&c, 1, 1), [10, 20, 30, 255]);
        assert_eq!(px(&c, 5, 5), [0, 0, 0, 0]);
        c.clear_rect(0.0, 0.0, 2.0, 2.0);
        assert_eq!(px(&c, 1, 1), [0, 0, 0, 0]);
        assert_eq!(px(&c, 3, 3), [10, 20, 30, 255]);
        // Solid regions are exact even with noise.
        let mut n = Canvas2d::new(
            Rc::new(PaintEnv {
                fonts: FontSet::new(None),
                noise: Some(CanvasNoise::new([3; 16])),
            }),
            4,
            4,
            true,
        );
        n.set_fill_style(Style::Color(parse_color("rgb(10,20,30)").unwrap()));
        n.fill_rect(0.0, 0.0, 4.0, 4.0);
        assert!(
            n.get_image_data(0, 0, 4, 4)
                .data
                .as_chunks::<4>()
                .0
                .iter()
                .all(|p| *p == [10, 20, 30, 255])
        );
    }

    /// The detect-bench `emoji_render` probe: an emoji drawn with a black
    /// fill style comes out in colour, sized by the persona's emoji font.
    #[test]
    fn emoji_draw_in_colour_for_every_reference_persona() {
        let dir = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../profiles/reference/chrome"
        );
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let p = PersonaProfile::from_path(&path).unwrap();
            let mut c = Canvas2d::new(Rc::new(PaintEnv::new(Some(&p))), 80, 70, true);
            c.set_fill_style(Style::Color(Color::BLACK));
            c.set_text_baseline(TextBaseline::Top);
            c.set_font("56px sans-serif");
            c.fill_text("\u{1F600}", 4.0, 4.0, None);
            let d = c.get_image_data(0, 0, 80, 70).data;
            let (mut inked, mut colored) = (0, 0);
            for px in d.as_chunks::<4>().0.iter() {
                if px[3] == 0 {
                    continue;
                }
                inked += 1;
                let (mx, mn) = (px[..3].iter().max().unwrap(), px[..3].iter().min().unwrap());
                if mx - mn > 40 {
                    colored += 1;
                }
            }
            assert!(
                colored > 1000 && inked >= colored,
                "{}: {inked} {colored}",
                path.display()
            );
            let m = c.measure_text("\u{1F600}");
            let want = 56.0 * lb_layout::emoji_advance_em(&p.font_defaults().emoji);
            assert!(
                (m.width - f64::from(want)).abs() < 1e-3,
                "{}",
                path.display()
            );
            assert!(m.actual_bounding_box_right > 0.0);
            // Text around it still uses the fill style.
            c.fill_text("A\u{1F600}", 4.0, 4.0, None);
        }
    }

    #[test]
    fn transforms_alpha_and_state() {
        let mut c = canvas(20, 20);
        c.save();
        c.translate(10.0, 10.0);
        c.scale(2.0, 2.0);
        c.set_global_alpha(0.5);
        c.set_fill_style(Style::Color(Color::rgb(255, 0, 0)));
        c.fill_rect(0.0, 0.0, 2.0, 2.0);
        c.restore();
        assert_eq!(c.global_alpha(), 1.0);
        assert_eq!(c.get_transform(), [1.0, 0.0, 0.0, 1.0, 0.0, 0.0]);
        assert_eq!(px(&c, 12, 12), [255, 0, 0, 128]);
        assert_eq!(px(&c, 9, 9), [0, 0, 0, 0]);
        c.rotate(std::f64::consts::FRAC_PI_2);
        let t = c.get_transform();
        assert!((t[1] - 1.0).abs() < 1e-6 && (t[2] + 1.0).abs() < 1e-6);
    }

    #[test]
    fn paths_stroke_clip_composite() {
        let mut c = canvas(40, 40);
        c.path_op(|p, ts| p.arc(ts, 20.0, 20.0, 10.0, 0.0, std::f64::consts::TAU, false));
        c.fill(None, FillRule::Winding);
        assert_eq!(px(&c, 20, 20)[3], 255);
        assert_eq!(px(&c, 2, 2)[3], 0);
        c.begin_path();
        c.set_line_width(4.0);
        c.set_stroke_style(Style::Color(Color::rgb(0, 0, 255)));
        c.path_op(|p, ts| {
            p.move_to(ts, 0.0, 35.0);
            p.line_to(ts, 40.0, 35.0);
        });
        c.stroke(None);
        assert_eq!(px(&c, 10, 35), [0, 0, 255, 255]);
        assert!(c.is_point_in_stroke(None, 10.0, 34.0));
        // Clip limits drawing.
        c.save();
        c.begin_path();
        c.path_op(|p, ts| p.rect(ts, 0.0, 0.0, 5.0, 5.0));
        c.clip(None, FillRule::Winding);
        c.set_fill_style(Style::Color(Color::rgb(0, 255, 0)));
        c.fill_rect(0.0, 0.0, 40.0, 40.0);
        c.restore();
        assert_eq!(px(&c, 2, 2), [0, 255, 0, 255]);
        assert_eq!(px(&c, 20, 20), [0, 0, 0, 255]);
        // `copy` clears outside the shape.
        c.set_composite("copy");
        c.fill_rect(30.0, 0.0, 5.0, 5.0);
        assert_eq!(px(&c, 20, 20), [0, 0, 0, 0]);
        assert_eq!(px(&c, 32, 2)[3], 255);
        assert_eq!(c.composite(), "copy");
    }

    #[test]
    fn gradients_patterns_shadows_text() {
        let mut c = canvas(100, 40);
        let g = Rc::new(Gradient::linear(0.0, 0.0, 100.0, 0.0));
        g.add_color_stop(0.0, Color::rgb(255, 0, 0));
        g.add_color_stop(1.0, Color::rgb(0, 0, 255));
        c.set_fill_style(Style::Gradient(g));
        c.fill_rect(0.0, 0.0, 100.0, 10.0);
        let (l, r) = (px(&c, 2, 5), px(&c, 97, 5));
        assert!(l[0] > 200 && r[2] > 200, "{l:?} {r:?}");
        // Pattern without repeat only covers the image.
        let mut tile = Pixmap::new(2, 2).unwrap();
        tile.fill(tiny_skia::Color::from_rgba8(0, 255, 0, 255));
        let pat = Rc::new(CanvasPattern::new(Rc::new(tile), Repetition::NoRepeat));
        pat.set_transform([1.0, 0.0, 0.0, 1.0, 50.0, 20.0]);
        c.set_fill_style(Style::Pattern(pat));
        c.fill_rect(0.0, 10.0, 100.0, 30.0);
        assert_eq!(px(&c, 51, 21), [0, 255, 0, 255]);
        assert_eq!(px(&c, 60, 30), [0, 0, 0, 0]);
        // Shadow.
        let mut s = canvas(40, 40);
        s.set_shadow_color(Color::rgb(0, 0, 0));
        s.set_shadow_offset_x(10.0);
        s.set_shadow_blur(2.0);
        s.fill_rect(5.0, 5.0, 10.0, 10.0);
        assert!(px(&s, 22, 10)[3] > 100);
        // Text draws something inside its measured box.
        let mut t = canvas(120, 30);
        t.set_font("20px Arial");
        let m = t.measure_text("Hello");
        assert!(m.width > 40.0 && m.width < 60.0);
        t.fill_text("Hello", 2.0, 22.0, None);
        let ink = t
            .read(0, 0, 120, 30, false)
            .data
            .as_chunks::<4>()
            .0
            .iter()
            .filter(|p| p[3] > 0)
            .count();
        assert!(ink > 50, "{ink}");
        assert_eq!(t.font(), "20px Arial");
    }

    #[test]
    fn image_data_round_trip_and_draw_image() {
        let mut c = canvas(4, 4);
        let mut b = Bitmap::new(2, 2);
        b.data
            .copy_from_slice(&[255, 0, 0, 255, 0, 255, 0, 128, 0, 0, 255, 255, 1, 2, 3, 0]);
        c.put_image_data(&b, 1, 1, None);
        let r = c.read(1, 1, 2, 2, false);
        assert_eq!(&r.data[..4], &[255, 0, 0, 255]);
        assert_eq!(r.data[7], 128);
        assert_eq!(&r.data[12..], &[0, 0, 0, 0]);
        // Out-of-bounds reads are transparent.
        assert_eq!(c.read(-1, -1, 1, 1, false).data, vec![0, 0, 0, 0]);
        let src = c.snapshot().unwrap();
        let mut d = canvas(8, 8);
        d.draw_image(&src, 0.0, 0.0, 4.0, 4.0, 0.0, 0.0, 8.0, 8.0);
        assert_eq!(px(&d, 2, 2)[0], 255);
        assert!(
            d.to_data_url("image/png", None)
                .starts_with("data:image/png;base64,")
        );
        // Resize clears.
        d.resize(3, 3);
        assert_eq!(px(&d, 1, 1), [0, 0, 0, 0]);
        assert_eq!(canvas(0, 5).to_data_url("", None), "data:,");
    }
}
