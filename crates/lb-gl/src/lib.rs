//! Software WebGL for LightBrowser (ADR 0030): a GLSL ES 1.00 / 3.00
//! compiler with ANGLE's messages, a lane-parallel interpreter, and the
//! rasterization pipeline (clipping, triangles, lines, points, depth,
//! stencil, blending, multisampling), textures and framebuffers.

// Numeric kernels index several parallel arrays by lane or component.
#![allow(
    clippy::needless_range_loop,
    clippy::too_many_arguments,
    clippy::type_complexity
)]

pub mod exec;
pub mod glsl;
pub mod image;
pub mod ir;
pub mod math;
pub mod program;
pub mod raster;
pub mod texture;
