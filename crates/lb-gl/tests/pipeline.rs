//! Draws through the whole pipeline.

use std::collections::HashMap;
use std::rc::Rc;

use lb_gl::glsl::{Options, Stage, compile};
use lb_gl::image::{Image, RGBA8};
use lb_gl::program::{LinkOptions, Program, link};
use lb_gl::raster::{AttribSrc, Draw, Fixed, Indices, Target, VertexArray, draw};
use lb_gl::texture::Units;

pub fn program(vs: &str, fs: &str) -> Program {
    let o = Options {
        webgl2: true,
        extensions: vec![],
        max_draw_buffers: 8,
    };
    let (v, l1) = compile(vs, Stage::Vertex, &o);
    let (f, l2) = compile(fs, Stage::Fragment, &o);
    let v = v.unwrap_or_else(|| panic!("vs: {l1}"));
    let f = f.unwrap_or_else(|| panic!("fs: {l2}"));
    link(
        Rc::new(v),
        Rc::new(f),
        &LinkOptions {
            max_attribs: 16,
            bound_locations: HashMap::new(),
        },
    )
    .expect("link")
}

pub fn fixed(w: i32, h: i32) -> Fixed {
    Fixed {
        viewport: [0, 0, w, h],
        depth_range: [0.0, 1.0],
        scissor: None,
        cull: None,
        front_ccw: true,
        polygon_offset: None,
        depth_func: None,
        depth_mask: true,
        stencil: None,
        blend: None,
        color_mask: [true; 4],
        rasterizer_discard: false,
        alpha_to_coverage: false,
        sample_coverage: None,
    }
}

fn bytes(v: &[f32]) -> Vec<u8> {
    v.iter().flat_map(|x| x.to_le_bytes()).collect()
}

fn dump(name: &str, img: &Image) {
    if let Ok(dir) = std::env::var("LB_GL_DUMP") {
        let px = img.resolve_rgba8(0);
        let mut out = format!(
            "P7\nWIDTH {}\nHEIGHT {}\nDEPTH 4\nMAXVAL 255\nTUPLTYPE RGB_ALPHA\nENDHDR\n",
            img.width, img.height
        )
        .into_bytes();
        for y in (0..img.height as usize).rev() {
            out.extend_from_slice(
                &px[y * img.width as usize * 4..(y + 1) * img.width as usize * 4],
            );
        }
        std::fs::write(format!("{dir}/{name}.pam"), out).expect("write");
    }
}

#[test]
fn gradient_triangle() {
    let p = program(
        "attribute vec2 pos; attribute vec3 col; varying vec3 c; void main() { c = col; gl_Position = vec4(pos, 0.0, 1.0); }",
        "precision mediump float; varying vec3 c; void main() { gl_FragColor = vec4(c, 1.0); }",
    );
    let pos = bytes(&[-0.8, -0.8, 0.8, -0.8, 0.0, 0.8]);
    let col = bytes(&[1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0]);
    let mut attribs = vec![AttribSrc::Value([0, 0, 0, 1f32.to_bits()]); 16];
    for a in &p.attribs {
        let (data, size) = if a.name == "pos" {
            (&pos, 2)
        } else {
            (&col, 3)
        };
        attribs[a.location as usize] = AttribSrc::Array(VertexArray {
            data,
            size,
            ty: 0x1406,
            normalized: false,
            integer: false,
            stride: 0,
            offset: 0,
            divisor: 0,
        });
    }
    let mut img = Image::new(RGBA8, 64, 64, 1, 4);
    let units = Units {
        units: vec![],
        webgl2: true,
        float_linear: false,
        feedback: vec![],
    };
    let fx = fixed(64, 64);
    let d = Draw {
        prog: &p,
        uniforms: &[],
        blocks: vec![],
        attribs,
        textures: &units,
        mode: 4,
        indices: Indices::Seq(0, 3),
        instances: 1,
        fixed: &fx,
        restart: None,
    };
    let mut t = Target {
        width: 64,
        height: 64,
        samples: 4,
        colors: vec![Some(&mut img)],
        depth: None,
        stencil: None,
    };
    let mut budget = 1 << 30;
    assert!(draw(&d, &mut t, &mut budget));
    dump("gradient", &img);
    let px = img.resolve_rgba8(0);
    let at = |x: usize, y: usize| &px[(y * 64 + x) * 4..(y * 64 + x) * 4 + 4];
    // Center: mixed colors, opaque.
    let c = at(32, 30);
    assert_eq!(c[3], 255);
    assert!(c[0] > 40 && c[1] > 40 && c[2] > 40, "{c:?}");
    // Near the bottom-left corner: mostly red.
    let r = at(9, 7);
    assert!(r[0] > 180 && r[1] < 60, "{r:?}");
    // Outside: transparent.
    assert_eq!(at(2, 60), &[0, 0, 0, 0]);
    // Some edge pixel has partial coverage (MSAA resolve).
    assert!((0..64).any(|x| {
        let a = at(x, 32)[3];
        a > 0 && a < 255
    }));
}
