//! Interpreter: compiled shaders computing known values.

use lb_gl::exec::{ALL, Exec, NoTextures};
use lb_gl::glsl::{Options, Stage, compile};
use lb_gl::ir::Shader;

fn vs(src: &str, webgl2: bool) -> Shader {
    let (s, log) = compile(
        src,
        Stage::Vertex,
        &Options {
            webgl2,
            extensions: vec![],
            max_draw_buffers: 8,
        },
    );
    s.unwrap_or_else(|| panic!("compile failed: {log}"))
}

/// Runs with one input attribute `a` (vec4 per lane = lane index) and
/// returns gl_Position of every lane.
fn run(s: &Shader) -> Vec<[f32; 4]> {
    let t = NoTextures;
    let mut e = Exec::new(s.cells, &t, 1 << 30);
    if let Some(a) = s.inputs.iter().find(|i| i.name == "a") {
        for l in 0..16 {
            for k in 0..4 {
                e.regs[(a.cell + k) as usize][l] = (l as f32 + k as f32 * 0.25).to_bits();
            }
        }
    }
    e.run(&s.code, ALL);
    assert!(!e.aborted);
    let p = s.builtins[&lb_gl::glsl::ast::Builtin::Position];
    (0..16)
        .map(|l| std::array::from_fn(|k| f32::from_bits(e.regs[p as usize + k][l])))
        .collect()
}

#[test]
fn arithmetic_functions_loops() {
    let s = vs(
        "attribute vec4 a;
         float sq(float x) { return x * x; }
         void twice(inout vec2 v, out float n) { v *= 2.0; n = length(v); }
         void main() {
           float s = 0.0;
           for (int i = 0; i < 4; i++) { if (i == 2) continue; s += float(i); }
           vec2 v = a.xy; float n; twice(v, n);
           mat2 m = mat2(1.0, 2.0, 3.0, 4.0);
           vec2 mv = m * vec2(1.0, 1.0);
           gl_Position = vec4(s + sq(a.x), v.y, mv.x + mv.y, n > 0.0 ? 1.0 : -1.0);
         }",
        false,
    );
    let out = run(&s);
    for (l, p) in out.iter().enumerate() {
        let x = l as f32;
        assert_eq!(p[0], 4.0 + x * x, "lane {l}");
        assert_eq!(p[1], 2.0 * (x + 0.25));
        assert_eq!(p[2], 10.0);
        assert_eq!(p[3], if x == 0.0 && false { -1.0 } else { 1.0 });
    }
}

#[test]
fn divergent_control_flow() {
    let s = vs(
        "#version 300 es
         in vec4 a;
         float f(float x) { if (x > 5.0) return 1.0; for (int i = 0; i < 10; i++) { if (float(i) >= x) return float(i) * 10.0; } return -1.0; }
         void main() {
           int k = int(a.x);
           float r = 0.0;
           switch (k % 3) { case 0: r = 100.0; break; case 1: r = 200.0; default: r += 5.0; }
           int j = 0; while (j < k) { j += 2; }
           float arr[4] = float[4](1.0, 2.0, 3.0, 4.0);
           arr[k % 4] = 9.0;
           gl_Position = vec4(f(a.x), r, float(j), arr[0] + arr[1] + arr[2] + arr[3]);
         }",
        true,
    );
    let out = run(&s);
    for (l, p) in out.iter().enumerate() {
        let x = l as f32;
        let fx = if x > 5.0 { 1.0 } else { x.ceil() * 10.0 };
        assert_eq!(p[0], fx, "lane {l}");
        let r = match l % 3 {
            0 => 100.0,
            1 => 205.0,
            _ => 5.0,
        };
        assert_eq!(p[1], r, "lane {l}");
        let j = (l as i32 + 1) / 2 * 2;
        assert_eq!(p[2], j as f32, "lane {l}");
        let mut arr = [1.0, 2.0, 3.0, 4.0];
        arr[l % 4] = 9.0;
        assert_eq!(p[3], arr.iter().sum::<f32>(), "lane {l}");
    }
}
