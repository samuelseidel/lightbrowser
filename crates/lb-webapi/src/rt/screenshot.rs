//! Read-only access to element pixels and control values for page
//! screenshots (lb-page `Page.captureScreenshot`). Nothing here is
//! visible to the page, and canvas pixels are never noised.

use std::rc::Rc;

use lb_dom::NodeId;
use lb_paint::Pixmap;

use super::Window;
use super::canvas::{CanvasCtx, Source, element_ctx, sync_element};
use super::image;

/// Pixels of a replaced element of the main document: a canvas's current
/// bitmap (2D or WebGL, without readback noise) or a loaded `<img>`'s
/// decoded pixels (premultiplied). `None` when there is nothing to draw.
pub fn element_pixels(w: &Window, node: NodeId) -> Option<Rc<Pixmap>> {
    let is_canvas = w.with_doc(|d| d.is_html_element_named(node, &lb_dom::local_name!("canvas")));
    if is_canvas {
        sync_element(w, 0, node);
        return match element_ctx(w, 0, node)? {
            CanvasCtx::TwoD { ctx, .. } => ctx.borrow().pixmap().cloned().map(Rc::new),
            CanvasCtx::External { ctx, .. } => {
                let bmp = ctx.raw.as_ref().and_then(|f| f())?;
                let mut data = bmp.data;
                lb_paint::decode::premultiply(&mut data);
                Pixmap::from_vec(
                    data,
                    lb_paint::tiny_skia::IntSize::from_wh(bmp.width, bmp.height)?,
                )
                .map(Rc::new)
            }
        };
    }
    match image::pixels(w, 0, node) {
        Ok(Source::Pixels(p, _)) => Some(p),
        _ => None,
    }
}

/// The current value of a text control of the main document (dirty value
/// or default), as `input.value` / `textarea.value` reads it.
pub fn control_value(w: &Window, node: NodeId) -> String {
    super::input::control_value(w, 0, node)
}
