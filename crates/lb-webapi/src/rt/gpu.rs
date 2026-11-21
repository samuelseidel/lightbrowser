//! `navigator.gpu` (WebGPU), from the persona's `webgpu()`.
//!
//! `requestAdapter()` resolves `null` when the persona has no adapter
//! (Linux by default, captured: no WebGPU on Linux Chrome 153), else a
//! `GPUAdapter` whose `info` names the persona's GPU vendor and
//! architecture. `getPreferredCanvasFormat()` is `rgba8unorm` on Linux
//! (captured) and `bgra8unorm` on Windows and macOS. Limits, features and
//! WGSL language features are the M7 surface reference (not captured: no
//! GPU here); no device can be created.

use lb_js::{Arg, CallCtx, HostError, ValueKind};
use lb_persona::{OsKind, WebGpuAdapter};

use super::navigator::{NavObj, new_nav_obj, single_nav, with_nav};
use super::{Runtime, Window, getter, method, need_args, this_obj};
use crate::registry::Registry;

/// WebGPU host objects.
pub(crate) enum GpuObj {
    Gpu,
    Wgsl,
    Adapter(Box<WebGpuAdapter>),
    Info(Box<WebGpuAdapter>),
    Features(bool),
    Limits,
}

impl GpuObj {
    pub(crate) fn iface_name(&self) -> &'static str {
        match self {
            GpuObj::Gpu => "GPU",
            GpuObj::Wgsl => "WGSLLanguageFeatures",
            GpuObj::Adapter(_) => "GPUAdapter",
            GpuObj::Info(_) => "GPUAdapterInfo",
            GpuObj::Features(_) => "GPUSupportedFeatures",
            GpuObj::Limits => "GPUSupportedLimits",
        }
    }
}

/// WGSL language features of Chrome 153.
const WGSL_FEATURES: &[&str] = &[
    "readonly_and_readwrite_storage_textures",
    "packed_4x8_integer_dot_product",
    "unrestricted_pointer_parameters",
    "pointer_composite_access",
];

/// Adapter features of a desktop GPU; Apple GPUs add ETC2 and ASTC.
const FEATURES: &[&str] = &[
    "core-features-and-limits",
    "depth-clip-control",
    "depth32float-stencil8",
    "texture-compression-bc",
    "texture-compression-bc-sliced-3d",
    "timestamp-query",
    "indirect-first-instance",
    "shader-f16",
    "rg11b10ufloat-renderable",
    "bgra8unorm-storage",
    "float32-filterable",
    "float32-blendable",
    "clip-distances",
    "dual-source-blending",
    "subgroups",
];
const APPLE_FEATURES: &[&str] = &[
    "texture-compression-etc2",
    "texture-compression-astc",
    "texture-compression-astc-sliced-3d",
];

/// `GPUSupportedLimits` in the fixture's order (Chrome's bucketed adapter
/// limits).
const LIMITS: &[(&str, f64)] = &[
    ("maxTextureDimension1D", 16384.0),
    ("maxTextureDimension2D", 16384.0),
    ("maxTextureDimension3D", 2048.0),
    ("maxTextureArrayLayers", 2048.0),
    ("maxBindGroups", 4.0),
    ("maxBindGroupsPlusVertexBuffers", 24.0),
    ("maxBindingsPerBindGroup", 1000.0),
    ("maxDynamicUniformBuffersPerPipelineLayout", 10.0),
    ("maxDynamicStorageBuffersPerPipelineLayout", 8.0),
    ("maxSampledTexturesPerShaderStage", 16.0),
    ("maxSamplersPerShaderStage", 16.0),
    ("maxStorageBuffersPerShaderStage", 10.0),
    ("maxStorageTexturesPerShaderStage", 8.0),
    ("maxUniformBuffersPerShaderStage", 12.0),
    ("maxUniformBufferBindingSize", 65536.0),
    ("maxStorageBufferBindingSize", 2_147_483_644.0),
    ("minUniformBufferOffsetAlignment", 256.0),
    ("minStorageBufferOffsetAlignment", 256.0),
    ("maxVertexBuffers", 8.0),
    ("maxBufferSize", 4_294_967_296.0),
    ("maxVertexAttributes", 30.0),
    ("maxVertexBufferArrayStride", 2048.0),
    ("maxInterStageShaderVariables", 28.0),
    ("maxColorAttachments", 8.0),
    ("maxColorAttachmentBytesPerSample", 64.0),
    ("maxComputeWorkgroupStorageSize", 32768.0),
    ("maxComputeInvocationsPerWorkgroup", 1024.0),
    ("maxComputeWorkgroupSizeX", 1024.0),
    ("maxComputeWorkgroupSizeY", 1024.0),
    ("maxComputeWorkgroupSizeZ", 64.0),
    ("maxComputeWorkgroupsPerDimension", 65535.0),
    ("maxImmediateSize", 64.0),
    ("maxStorageBuffersInFragmentStage", 10.0),
    ("maxStorageTexturesInFragmentStage", 8.0),
    ("maxStorageBuffersInVertexStage", 10.0),
    ("maxStorageTexturesInVertexStage", 8.0),
];

/// Subgroup sizes by vendor.
fn subgroup_sizes(vendor: &str) -> (f64, f64) {
    match vendor {
        "amd" => (32.0, 64.0),
        "intel" => (8.0, 32.0),
        "qualcomm" => (64.0, 128.0),
        _ => (32.0, 32.0),
    }
}

fn gpu_obj<R>(w: &Window, id: u32, f: impl FnOnce(&GpuObj) -> Option<R>) -> Option<R> {
    with_nav(w, id, |o| match o {
        NavObj::Gpu(g) => f(g),
        _ => None,
    })
}

/// `this` as a GPU object `f` accepts.
fn this_gpu<R>(
    cx: &mut CallCtx<'_, '_, '_>,
    w: &Window,
    f: impl FnOnce(&GpuObj) -> Option<R>,
) -> Result<(u32, R), HostError> {
    let id = this_obj(cx)?;
    gpu_obj(w, id, f)
        .map(|r| (id, r))
        .ok_or_else(super::illegal)
}

/// A child object of `parent` that is the same on every access.
fn child(w: &Window, key: &'static str, parent: u32, make: impl FnOnce() -> GpuObj) -> Arg {
    let id = w.cached_obj((parent, 0, key, String::new()), || {
        super::Obj::Nav(Box::new(NavObj::Gpu(Box::new(make()))))
    });
    w.obj_arg(id)
}

/// Items of a setlike (`GPUSupportedFeatures` of an Apple GPU or not,
/// `WGSLLanguageFeatures`).
fn set_items(o: &GpuObj) -> Option<Vec<&'static str>> {
    match o {
        GpuObj::Wgsl => Some(WGSL_FEATURES.to_vec()),
        GpuObj::Features(apple) => {
            let mut v = FEATURES.to_vec();
            if *apple {
                v.extend_from_slice(APPLE_FEATURES);
            }
            Some(v)
        }
        _ => None,
    }
}

fn register_setlike(reg: &mut Registry, rt: &Runtime, iface: &'static str) {
    getter(reg, rt, iface, "size", |cx, w| {
        let (_, items) = this_gpu(cx, w, set_items)?;
        cx.ret(items.len() as f64)
    });
    method(reg, rt, iface, "has", move |cx, w| {
        let (_, items) = this_gpu(cx, w, set_items)?;
        need_args(cx, 1, "has", iface)?;
        let k = cx.arg_string(0)?;
        cx.ret(items.contains(&k.as_str()))
    });
    for (op, pairs) in [("entries", true), ("keys", false), ("values", false)] {
        method(reg, rt, iface, op, move |cx, w| {
            let (_, items) = this_gpu(cx, w, set_items)?;
            let args = items
                .into_iter()
                .map(|s| {
                    if pairs {
                        Arg::Array(vec![Arg::from(s), Arg::from(s)])
                    } else {
                        Arg::from(s)
                    }
                })
                .collect();
            super::misc::ret_iterator(cx, w, args)
        });
    }
    method(reg, rt, iface, "forEach", move |cx, w| {
        let (id, items) = this_gpu(cx, w, set_items)?;
        need_args(cx, 1, "forEach", iface)?;
        if cx.arg_kind(0) != ValueKind::Function {
            return Err(HostError::type_error(format!(
                "Failed to execute 'forEach' on '{iface}': The callback provided as parameter 1 is not a function."
            )));
        }
        let f = cx.arg_handle(0);
        let this = if cx.len() > 1 {
            Arg::Handle(cx.arg_handle(1))
        } else {
            Arg::Undefined
        };
        for s in items {
            cx.call(
                f,
                this.clone(),
                &[Arg::from(s), Arg::from(s), w.obj_arg(id)],
            )?;
        }
        Ok(())
    });
}

pub(crate) fn register(reg: &mut Registry, rt: &Runtime) {
    getter(reg, rt, "Navigator", "gpu", |cx, w| {
        this_obj(cx)?;
        cx.ret(single_nav(w, "nav:gpu", NavObj::Gpu(Box::new(GpuObj::Gpu))))
    });
    let is_gpu = |o: &GpuObj| matches!(o, GpuObj::Gpu).then_some(());
    getter(reg, rt, "GPU", "wgslLanguageFeatures", move |cx, w| {
        let (id, ()) = this_gpu(cx, w, is_gpu)?;
        cx.ret(child(w, "gpu:wgsl", id, || GpuObj::Wgsl))
    });
    method(reg, rt, "GPU", "getPreferredCanvasFormat", move |cx, w| {
        this_gpu(cx, w, is_gpu)?;
        let linux = w
            .profile
            .as_deref()
            .is_some_and(|p| p.os_kind() == OsKind::Linux);
        cx.ret(if linux { "rgba8unorm" } else { "bgra8unorm" })
    });
    // A new adapter per call, or null.
    method(reg, rt, "GPU", "requestAdapter", move |cx, w| {
        this_gpu(cx, w, is_gpu)?;
        let r = cx.ret_new_promise()?;
        let adapter = w.profile.as_deref().and_then(|p| p.webgpu());
        let v = match adapter {
            Some(a) => new_nav_obj(w, NavObj::Gpu(Box::new(GpuObj::Adapter(Box::new(a))))),
            None => Arg::Null,
        };
        cx.resolve_promise(r, v)
    });
    let adapter = |o: &GpuObj| match o {
        GpuObj::Adapter(a) => Some((**a).clone()),
        _ => None,
    };
    getter(reg, rt, "GPUAdapter", "info", move |cx, w| {
        let (id, a) = this_gpu(cx, w, adapter)?;
        cx.ret(child(w, "gpu:info", id, || GpuObj::Info(Box::new(a))))
    });
    getter(reg, rt, "GPUAdapter", "features", move |cx, w| {
        let (id, a) = this_gpu(cx, w, adapter)?;
        let apple = a.vendor == "apple";
        cx.ret(child(w, "gpu:features", id, || GpuObj::Features(apple)))
    });
    getter(reg, rt, "GPUAdapter", "limits", move |cx, w| {
        let (id, _) = this_gpu(cx, w, adapter)?;
        cx.ret(child(w, "gpu:limits", id, || GpuObj::Limits))
    });
    method(reg, rt, "GPUAdapter", "requestDevice", move |cx, w| {
        this_gpu(cx, w, adapter)?;
        let r = cx.ret_new_promise()?;
        cx.reject_promise(
            r,
            w.rt.err(
                "OperationError",
                "Failed to execute 'requestDevice' on 'GPUAdapter': Failed to create device.",
            ),
        )
    });
    let info = |o: &GpuObj| match o {
        GpuObj::Info(a) => Some((**a).clone()),
        _ => None,
    };
    for name in ["vendor", "architecture", "device", "description"] {
        getter(reg, rt, "GPUAdapterInfo", name, move |cx, w| {
            let (_, a) = this_gpu(cx, w, info)?;
            cx.ret(match name {
                "vendor" => a.vendor,
                "architecture" => a.architecture,
                "device" => a.device,
                _ => a.description,
            })
        });
    }
    getter(
        reg,
        rt,
        "GPUAdapterInfo",
        "subgroupMinSize",
        move |cx, w| {
            let (_, a) = this_gpu(cx, w, info)?;
            cx.ret(subgroup_sizes(&a.vendor).0)
        },
    );
    getter(
        reg,
        rt,
        "GPUAdapterInfo",
        "subgroupMaxSize",
        move |cx, w| {
            let (_, a) = this_gpu(cx, w, info)?;
            cx.ret(subgroup_sizes(&a.vendor).1)
        },
    );
    getter(
        reg,
        rt,
        "GPUAdapterInfo",
        "isFallbackAdapter",
        move |cx, w| {
            this_gpu(cx, w, info)?;
            cx.ret(false)
        },
    );
    for &(name, v) in LIMITS {
        getter(reg, rt, "GPUSupportedLimits", name, move |cx, w| {
            this_gpu(cx, w, |o| matches!(o, GpuObj::Limits).then_some(()))?;
            cx.ret(v)
        });
    }
    register_setlike(reg, rt, "GPUSupportedFeatures");
    register_setlike(reg, rt, "WGSLLanguageFeatures");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The limit table names every `GPUSupportedLimits` attribute of the
    /// generated table, in order.
    #[test]
    fn limits_cover_the_interface() {
        let d = crate::generated::chromium::TABLE
            .interface("GPUSupportedLimits")
            .unwrap();
        let names: Vec<&str> = d.proto_order.iter().map(|s| d.slot_name(*s)).collect();
        let ours: Vec<&str> = LIMITS.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ours);
    }
}
