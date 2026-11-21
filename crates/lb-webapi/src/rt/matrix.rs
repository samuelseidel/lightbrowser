//! `DOMMatrixReadOnly` / `DOMMatrix` (what `getTransform()` returns), plus
//! JS number formatting for messages and `toString()`.
//!
//! Supported: constructors from nothing, 6/16-number sequences and
//! `matrix()`/`matrix3d()`/`none` strings; all component attributes,
//! `is2D`, `isIdentity`, `multiply`, `inverse`, `translate`, `scale`,
//! `rotate`, `flipX`, `flipY`, `toString`, `toJSON`, the matching `*Self`
//! methods and `fromMatrix`. Other members are stubs.

use lb_js::{Arg, CallCtx, HostError, HostRef, ValueKind};

use super::canvas::{PaintObj, new_paint, paint};
use super::{Dict, K_OBJ, Runtime, Window, getter, illegal, method, setter, this_obj};
use crate::registry::{ImplKind, Registry};

/// A 4x4 matrix: `m[(r - 1) * 4 + (c - 1)]` is `mRC` (`m41` = `e`).
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Matrix {
    m: [f64; 16],
    is_2d: bool,
    readonly: bool,
}

const ID: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

/// Indices of `a`..`f` in `m`.
const IDX_2D: [usize; 6] = [0, 1, 4, 5, 12, 13];

const NAMES: [&str; 16] = [
    "m11", "m12", "m13", "m14", "m21", "m22", "m23", "m24", "m31", "m32", "m33", "m34", "m41",
    "m42", "m43", "m44",
];

impl Matrix {
    pub(crate) fn from_2d(v: [f64; 6], readonly: bool) -> Matrix {
        let mut m = ID;
        for (k, i) in IDX_2D.iter().enumerate() {
            m[*i] = v[k];
        }
        Matrix {
            m,
            is_2d: true,
            readonly,
        }
    }

    pub(crate) fn to_2d(&self) -> [f64; 6] {
        IDX_2D.map(|i| self.m[i])
    }

    pub(crate) fn iface_name(&self) -> &'static str {
        if self.readonly {
            "DOMMatrixReadOnly"
        } else {
            "DOMMatrix"
        }
    }

    fn identity(&self) -> bool {
        self.m == ID
    }

    /// `self · other` (other applied first).
    fn multiply(&self, o: &Matrix) -> Matrix {
        let mut r = [0.0; 16];
        for row in 0..4 {
            for col in 0..4 {
                r[row * 4 + col] = (0..4).map(|k| o.m[row * 4 + k] * self.m[k * 4 + col]).sum();
            }
        }
        Matrix {
            m: r,
            is_2d: self.is_2d && o.is_2d,
            readonly: self.readonly,
        }
    }

    fn inverse(&self) -> Matrix {
        let m = &self.m;
        let mut inv = [0.0; 16];
        inv[0] = m[5] * m[10] * m[15] - m[5] * m[11] * m[14] - m[9] * m[6] * m[15]
            + m[9] * m[7] * m[14]
            + m[13] * m[6] * m[11]
            - m[13] * m[7] * m[10];
        inv[4] = -m[4] * m[10] * m[15] + m[4] * m[11] * m[14] + m[8] * m[6] * m[15]
            - m[8] * m[7] * m[14]
            - m[12] * m[6] * m[11]
            + m[12] * m[7] * m[10];
        inv[8] = m[4] * m[9] * m[15] - m[4] * m[11] * m[13] - m[8] * m[5] * m[15]
            + m[8] * m[7] * m[13]
            + m[12] * m[5] * m[11]
            - m[12] * m[7] * m[9];
        inv[12] = -m[4] * m[9] * m[14] + m[4] * m[10] * m[13] + m[8] * m[5] * m[14]
            - m[8] * m[6] * m[13]
            - m[12] * m[5] * m[10]
            + m[12] * m[6] * m[9];
        inv[1] = -m[1] * m[10] * m[15] + m[1] * m[11] * m[14] + m[9] * m[2] * m[15]
            - m[9] * m[3] * m[14]
            - m[13] * m[2] * m[11]
            + m[13] * m[3] * m[10];
        inv[5] = m[0] * m[10] * m[15] - m[0] * m[11] * m[14] - m[8] * m[2] * m[15]
            + m[8] * m[3] * m[14]
            + m[12] * m[2] * m[11]
            - m[12] * m[3] * m[10];
        inv[9] = -m[0] * m[9] * m[15] + m[0] * m[11] * m[13] + m[8] * m[1] * m[15]
            - m[8] * m[3] * m[13]
            - m[12] * m[1] * m[11]
            + m[12] * m[3] * m[9];
        inv[13] = m[0] * m[9] * m[14] - m[0] * m[10] * m[13] - m[8] * m[1] * m[14]
            + m[8] * m[2] * m[13]
            + m[12] * m[1] * m[10]
            - m[12] * m[2] * m[9];
        inv[2] = m[1] * m[6] * m[15] - m[1] * m[7] * m[14] - m[5] * m[2] * m[15]
            + m[5] * m[3] * m[14]
            + m[13] * m[2] * m[7]
            - m[13] * m[3] * m[6];
        inv[6] = -m[0] * m[6] * m[15] + m[0] * m[7] * m[14] + m[4] * m[2] * m[15]
            - m[4] * m[3] * m[14]
            - m[12] * m[2] * m[7]
            + m[12] * m[3] * m[6];
        inv[10] = m[0] * m[5] * m[15] - m[0] * m[7] * m[13] - m[4] * m[1] * m[15]
            + m[4] * m[3] * m[13]
            + m[12] * m[1] * m[7]
            - m[12] * m[3] * m[5];
        inv[14] = -m[0] * m[5] * m[14] + m[0] * m[6] * m[13] + m[4] * m[1] * m[14]
            - m[4] * m[2] * m[13]
            - m[12] * m[1] * m[6]
            + m[12] * m[2] * m[5];
        inv[3] = -m[1] * m[6] * m[11] + m[1] * m[7] * m[10] + m[5] * m[2] * m[11]
            - m[5] * m[3] * m[10]
            - m[9] * m[2] * m[7]
            + m[9] * m[3] * m[6];
        inv[7] = m[0] * m[6] * m[11] - m[0] * m[7] * m[10] - m[4] * m[2] * m[11]
            + m[4] * m[3] * m[10]
            + m[8] * m[2] * m[7]
            - m[8] * m[3] * m[6];
        inv[11] = -m[0] * m[5] * m[11] + m[0] * m[7] * m[9] + m[4] * m[1] * m[11]
            - m[4] * m[3] * m[9]
            - m[8] * m[1] * m[7]
            + m[8] * m[3] * m[5];
        inv[15] = m[0] * m[5] * m[10] - m[0] * m[6] * m[9] - m[4] * m[1] * m[10]
            + m[4] * m[2] * m[9]
            + m[8] * m[1] * m[6]
            - m[8] * m[2] * m[5];
        let det = m[0] * inv[0] + m[1] * inv[4] + m[2] * inv[8] + m[3] * inv[12];
        if det == 0.0 || !det.is_finite() {
            return Matrix {
                m: [f64::NAN; 16],
                is_2d: false,
                readonly: self.readonly,
            };
        }
        Matrix {
            m: inv.map(|v| v / det),
            is_2d: self.is_2d,
            readonly: self.readonly,
        }
    }

    fn translation(tx: f64, ty: f64, tz: f64) -> Matrix {
        let mut m = ID;
        m[12] = tx;
        m[13] = ty;
        m[14] = tz;
        Matrix {
            m,
            is_2d: tz == 0.0,
            readonly: false,
        }
    }

    fn scaling(sx: f64, sy: f64, sz: f64) -> Matrix {
        let mut m = ID;
        m[0] = sx;
        m[5] = sy;
        m[10] = sz;
        Matrix {
            m,
            is_2d: sz == 1.0,
            readonly: false,
        }
    }

    /// Rotation by `deg` around axis 0 (x), 1 (y) or 2 (z).
    fn rotation(axis: usize, deg: f64) -> Matrix {
        let (s, c) = deg.to_radians().sin_cos();
        let mut m = ID;
        let (i, j) = match axis {
            0 => (5, 10),
            1 => (10, 0),
            _ => (0, 5),
        };
        let row = |k: usize| k / 4;
        let col = |k: usize| k % 4;
        let (a, b) = (row(i), row(j));
        m[a * 4 + a] = c;
        m[b * 4 + b] = c;
        m[a * 4 + b] = s;
        m[b * 4 + a] = -s;
        let _ = col;
        Matrix {
            m,
            is_2d: axis == 2,
            readonly: false,
        }
    }

    fn to_css(&self) -> Option<String> {
        if self.m.iter().any(|v| !v.is_finite()) {
            return None;
        }
        Some(if self.is_2d {
            let v: Vec<String> = self.to_2d().iter().map(|v| js_num(*v)).collect();
            format!("matrix({})", v.join(", "))
        } else {
            let v: Vec<String> = self.m.iter().map(|v| js_num(*v)).collect();
            format!("matrix3d({})", v.join(", "))
        })
    }

    fn to_json(&self) -> String {
        let mut parts: Vec<String> = ["a", "b", "c", "d", "e", "f"]
            .iter()
            .zip(IDX_2D)
            .map(|(n, i)| format!("\"{n}\":{}", json_num(self.m[i])))
            .collect();
        for (k, n) in NAMES.iter().enumerate() {
            parts.push(format!("\"{n}\":{}", json_num(self.m[k])));
        }
        parts.push(format!("\"is2D\":{}", self.is_2d));
        parts.push(format!("\"isIdentity\":{}", self.identity()));
        format!("{{{}}}", parts.join(","))
    }
}

fn json_num(v: f64) -> String {
    if v.is_finite() {
        js_num(v)
    } else {
        "null".into()
    }
}

/// JS `Number.prototype.toString()` for a double.
pub(crate) fn js_num(v: f64) -> String {
    if v.is_nan() {
        return "NaN".into();
    }
    if v.is_infinite() {
        return if v > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if v == 0.0 {
        return "0".into();
    }
    // Shortest round-trip digits and exponent from Rust's formatter.
    let e = format!("{:e}", v.abs());
    let (mant, exp) = e.split_once('e').unwrap_or((&e, "0"));
    let digits: String = mant.chars().filter(char::is_ascii_digit).collect();
    let exp: i32 = exp.parse().unwrap_or(0);
    let k = digits.len() as i32;
    let n = exp + 1;
    let sign = if v < 0.0 { "-" } else { "" };
    let body = if k <= n && n <= 21 {
        format!("{digits}{}", "0".repeat((n - k) as usize))
    } else if 0 < n && n <= 21 {
        format!("{}.{}", &digits[..n as usize], &digits[n as usize..])
    } else if -6 < n && n <= 0 {
        format!("0.{}{digits}", "0".repeat((-n) as usize))
    } else {
        let e = n - 1;
        let es = if e >= 0 {
            format!("+{e}")
        } else {
            e.to_string()
        };
        if k == 1 {
            format!("{digits}e{es}")
        } else {
            format!("{}.{}e{es}", &digits[..1], &digits[1..])
        }
    };
    format!("{sign}{body}")
}

fn this_matrix(cx: &mut CallCtx<'_, '_, '_>, w: &Window) -> Result<(u32, Matrix), HostError> {
    let id = this_obj(cx)?;
    let m = paint(w, id, |p| match p {
        PaintObj::Matrix(m) => Some((**m).clone()),
        _ => None,
    })
    .ok_or_else(illegal)?;
    Ok((id, m))
}

fn store(w: &Window, id: u32, m: Matrix) {
    paint(w, id, move |p| match p {
        PaintObj::Matrix(old) => {
            let ro = old.readonly;
            **old = Matrix { readonly: ro, ..m };
            Some(())
        }
        _ => None,
    });
}

fn ret_matrix(cx: &mut CallCtx<'_, '_, '_>, w: &Window, m: Matrix) -> Result<(), HostError> {
    let id = new_paint(w, PaintObj::Matrix(Box::new(m)));
    cx.ret(w.obj_arg(id))
}

/// A `DOMMatrixInit` argument (DOMMatrix object or dictionary).
fn matrix_init(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    i: usize,
    op: &str,
) -> Result<Matrix, HostError> {
    if let Some(h) = cx.arg_host(i).filter(|h| h.kind == K_OBJ)
        && let Some(m) = paint(w, h.id, |p| match p {
            PaintObj::Matrix(m) => Some((**m).clone()),
            _ => None,
        })
    {
        return Ok(Matrix {
            readonly: false,
            ..m
        });
    }
    if i < cx.len()
        && !matches!(
            cx.arg_kind(i),
            ValueKind::Undefined | ValueKind::Null | ValueKind::Object | ValueKind::Function
        )
    {
        return Err(HostError::type_error(format!(
            "Failed to execute '{op}' on 'DOMMatrixReadOnly': The provided value is not of type 'DOMMatrixInit'."
        )));
    }
    let d = Dict::arg(cx, i);
    let base = super::canvas::matrix_2d_init(cx, w, i)?;
    let mut m = Matrix::from_2d(base, false);
    for (k, n) in NAMES.iter().enumerate() {
        if IDX_2D.contains(&k) {
            continue;
        }
        if let Some(v) = d.number(cx, n)? {
            m.m[k] = v;
        }
    }
    let explicit_2d = match d.get(cx, "is2D")? {
        Some(h) => {
            let b = super::truthy(&cx.handle_value(h));
            cx.release(h);
            Some(b)
        }
        None => None,
    };
    d.release(cx);
    let flat = [2usize, 3, 6, 7, 8, 9, 11, 14]
        .iter()
        .all(|k| m.m[*k] == 0.0)
        && m.m[10] == 1.0
        && m.m[15] == 1.0;
    m.is_2d = explicit_2d.unwrap_or(flat);
    Ok(m)
}

/// Constructor argument: nothing, a sequence or a transform string.
fn construct(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    iface: &str,
    readonly: bool,
) -> Result<Matrix, HostError> {
    let mut m = Matrix::from_2d([1.0, 0.0, 0.0, 1.0, 0.0, 0.0], readonly);
    if cx.is_empty() || cx.arg_kind(0) == ValueKind::Undefined {
        return Ok(m);
    }
    if cx.arg_kind(0) == ValueKind::Object {
        let h = cx.arg_handle(0);
        let len = cx.get_property(h, "length")?;
        let n = match cx.handle_value(len) {
            lb_js::JsValue::Number(n) => n as usize,
            _ => usize::MAX,
        };
        cx.release(len);
        let mut v = Vec::new();
        if n == 6 || n == 16 {
            for i in 0..n {
                let x = cx.get_property(h, &i.to_string())?;
                let f = match cx.handle_value(x) {
                    lb_js::JsValue::Number(f) => f,
                    lb_js::JsValue::String(s) => s.trim().parse().unwrap_or(f64::NAN),
                    _ => f64::NAN,
                };
                cx.release(x);
                v.push(f);
            }
        }
        return match v.len() {
            6 => Ok(Matrix::from_2d(
                [v[0], v[1], v[2], v[3], v[4], v[5]],
                readonly,
            )),
            16 => {
                m.m.copy_from_slice(&v);
                m.is_2d = false;
                Ok(m)
            }
            _ => Err(HostError::type_error(format!(
                "Failed to construct '{iface}': The sequence must contain 6 elements for a 2D matrix or 16 elements for a 3D matrix."
            ))),
        };
    }
    let s = cx.arg_string(0)?;
    let t = s.trim();
    if t.is_empty() || t.eq_ignore_ascii_case("none") {
        return Ok(m);
    }
    let lower = t.to_ascii_lowercase();
    let parse = |inner: &str| -> Option<Vec<f64>> {
        inner
            .split(',')
            .map(|p| p.trim().parse::<f64>().ok().filter(|f| f.is_finite()))
            .collect()
    };
    let bad = || {
        w.rt.err(
            "SyntaxError",
            format!("Failed to construct '{iface}': Failed to parse '{s}'."),
        )
    };
    if let Some(inner) = lower
        .strip_prefix("matrix(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let v = parse(inner).filter(|v| v.len() == 6).ok_or_else(bad)?;
        return Ok(Matrix::from_2d(
            [v[0], v[1], v[2], v[3], v[4], v[5]],
            readonly,
        ));
    }
    if let Some(inner) = lower
        .strip_prefix("matrix3d(")
        .and_then(|r| r.strip_suffix(')'))
    {
        let v = parse(inner).filter(|v| v.len() == 16).ok_or_else(bad)?;
        m.m.copy_from_slice(&v);
        m.is_2d = false;
        return Ok(m);
    }
    Err(bad())
}

fn opt_num(cx: &mut CallCtx<'_, '_, '_>, i: usize, def: f64) -> Result<f64, HostError> {
    if i < cx.len() && cx.arg_kind(i) != ValueKind::Undefined {
        cx.arg_f64(i)
    } else {
        Ok(def)
    }
}

/// The operation shared by `x()` and `xSelf()`.
fn op(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    name: &str,
    m: &Matrix,
) -> Result<Matrix, HostError> {
    Ok(match name {
        "multiply" => m.multiply(&matrix_init(cx, w, 0, "multiply")?),
        "preMultiply" => {
            let o = matrix_init(cx, w, 0, "preMultiplySelf")?;
            Matrix {
                readonly: m.readonly,
                ..o.multiply(m)
            }
        }
        "inverse" | "invert" => m.inverse(),
        "translate" => {
            let (tx, ty, tz) = (
                opt_num(cx, 0, 0.0)?,
                opt_num(cx, 1, 0.0)?,
                opt_num(cx, 2, 0.0)?,
            );
            m.multiply(&Matrix::translation(tx, ty, tz))
        }
        "scale" => {
            let sx = opt_num(cx, 0, 1.0)?;
            let sy = opt_num(cx, 1, sx)?;
            let sz = opt_num(cx, 2, 1.0)?;
            let (ox, oy, oz) = (
                opt_num(cx, 3, 0.0)?,
                opt_num(cx, 4, 0.0)?,
                opt_num(cx, 5, 0.0)?,
            );
            m.multiply(&Matrix::translation(ox, oy, oz))
                .multiply(&Matrix::scaling(sx, sy, sz))
                .multiply(&Matrix::translation(-ox, -oy, -oz))
        }
        "rotate" => {
            let a = opt_num(cx, 0, 0.0)?;
            let (rx, ry, rz) = if cx.len() < 2 || cx.arg_kind(1) == ValueKind::Undefined {
                (0.0, 0.0, a)
            } else {
                (a, opt_num(cx, 1, 0.0)?, opt_num(cx, 2, 0.0)?)
            };
            let mut r = m.multiply(&Matrix::rotation(2, rz));
            if ry != 0.0 {
                r = r.multiply(&Matrix::rotation(1, ry));
            }
            if rx != 0.0 {
                r = r.multiply(&Matrix::rotation(0, rx));
            }
            r
        }
        "flipX" => m.multiply(&Matrix::scaling(-1.0, 1.0, 1.0)),
        "flipY" => m.multiply(&Matrix::scaling(1.0, -1.0, 1.0)),
        _ => m.clone(),
    })
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    for (iface, readonly) in [("DOMMatrixReadOnly", true), ("DOMMatrix", false)] {
        super::ctor(reg, rt, iface, move |cx, w| {
            let m = construct(cx, w, iface, readonly)?;
            let id = new_paint(w, PaintObj::Matrix(Box::new(m)));
            cx.init_this(HostRef::new(K_OBJ, id))
        });
        for (k, name) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
            getter(reg, rt, iface, name, move |cx, w| {
                let (_, m) = this_matrix(cx, w)?;
                cx.ret(m.m[IDX_2D[k]])
            });
        }
        for (k, name) in NAMES.into_iter().enumerate() {
            getter(reg, rt, iface, name, move |cx, w| {
                let (_, m) = this_matrix(cx, w)?;
                cx.ret(m.m[k])
            });
        }
        reg.register(
            iface,
            "fromMatrix",
            ImplKind::StaticMethod,
            super::wf(rt, move |cx, w| {
                let m = matrix_init(cx, w, 0, "fromMatrix")?;
                ret_matrix(cx, w, Matrix { readonly, ..m })
            }),
        );
    }
    const RO: &str = "DOMMatrixReadOnly";
    getter(reg, rt, RO, "is2D", |cx, w| {
        let (_, m) = this_matrix(cx, w)?;
        cx.ret(m.is_2d)
    });
    getter(reg, rt, RO, "isIdentity", |cx, w| {
        let (_, m) = this_matrix(cx, w)?;
        cx.ret(m.identity())
    });
    method(reg, rt, RO, "toString", |cx, w| {
        let (_, m) = this_matrix(cx, w)?;
        match m.to_css() {
            Some(s) => cx.ret(s),
            None => Err(w.rt.err(
                "InvalidStateError",
                "Failed to execute 'toString' on 'DOMMatrixReadOnly': Cannot be serialized with NaN or Infinity values.",
            )),
        }
    });
    method(reg, rt, RO, "toJSON", |cx, w| {
        let (_, m) = this_matrix(cx, w)?;
        cx.ret(Arg::Json(m.to_json()))
    });
    for name in [
        "multiply",
        "inverse",
        "translate",
        "scale",
        "rotate",
        "flipX",
        "flipY",
    ] {
        method(reg, rt, RO, name, move |cx, w| {
            let (_, m) = this_matrix(cx, w)?;
            let r = op(cx, w, name, &m)?;
            ret_matrix(
                cx,
                w,
                Matrix {
                    readonly: false,
                    ..r
                },
            )
        });
    }
    const M: &str = "DOMMatrix";
    for (k, name) in ["a", "b", "c", "d", "e", "f"].into_iter().enumerate() {
        setter(reg, rt, M, name, move |cx, w| {
            let (id, mut m) = this_matrix(cx, w)?;
            m.m[IDX_2D[k]] = cx.arg_f64(0)?;
            store(w, id, m);
            Ok(())
        });
    }
    for (k, name) in NAMES.into_iter().enumerate() {
        setter(reg, rt, M, name, move |cx, w| {
            let (id, mut m) = this_matrix(cx, w)?;
            let v = cx.arg_f64(0)?;
            m.m[k] = v;
            if !IDX_2D.contains(&k) && v != if k == 10 || k == 15 { 1.0 } else { 0.0 } {
                m.is_2d = false;
            }
            store(w, id, m);
            Ok(())
        });
    }
    for (name, opname) in [
        ("multiplySelf", "multiply"),
        ("preMultiplySelf", "preMultiply"),
        ("invertSelf", "invert"),
        ("translateSelf", "translate"),
        ("scaleSelf", "scale"),
        ("rotateSelf", "rotate"),
    ] {
        method(reg, rt, M, name, move |cx, w| {
            let (id, m) = this_matrix(cx, w)?;
            let r = op(cx, w, opname, &m)?;
            store(w, id, r);
            let this = cx.this_handle();
            cx.ret(Arg::Handle(this))
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_numbers() {
        for (v, s) in [
            (1.0, "1"),
            (0.5, "0.5"),
            (-2.25, "-2.25"),
            (1e21, "1e+21"),
            (123456789.0, "123456789"),
            (1e-7, "1e-7"),
            (0.000001, "0.000001"),
            (1.5e-10, "1.5e-10"),
            (6.123233995736766e-17, "6.123233995736766e-17"),
            (0.1 + 0.2, "0.30000000000000004"),
        ] {
            assert_eq!(js_num(v), s);
        }
    }

    #[test]
    fn matrix_math() {
        let t = Matrix::from_2d([1.0, 0.0, 0.0, 1.0, 10.0, 0.0], false);
        let s = Matrix::scaling(2.0, 2.0, 1.0);
        assert_eq!(t.multiply(&s).to_2d(), [2.0, 0.0, 0.0, 2.0, 10.0, 0.0]);
        let inv = t.multiply(&s).inverse();
        assert_eq!(inv.to_2d(), [0.5, 0.0, 0.0, 0.5, -5.0, 0.0]);
        assert_eq!(
            Matrix::from_2d([2.0, 0.0, 0.0, 2.0, 1.0, 1.0], false)
                .to_css()
                .unwrap(),
            "matrix(2, 0, 0, 2, 1, 1)"
        );
        let r = Matrix::rotation(2, 90.0).to_2d();
        assert!((r[1] - 1.0).abs() < 1e-12 && (r[2] + 1.0).abs() < 1e-12);
    }
}
