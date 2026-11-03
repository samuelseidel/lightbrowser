//! [`DisplayList`]: the laid-out boxes in paint order, for painting a page
//! (CDP `Page.captureScreenshot`, painted by lb-paint).
//!
//! Geometry is in page coordinates: CSS px from the document origin, as
//! seen at the current scroll (client rect plus the viewport scroll), so
//! fixed boxes sit where the viewport shows them. Paint order follows CSS
//! 2.1 Appendix E as simplified for hit testing: per stacking context its
//! own box, negative z-index contexts, in-flow boxes in tree order,
//! positioned `z-index: auto/0` boxes, positive z-index contexts. A
//! stacking context with `opacity < 1` is wrapped in a group.

use std::rc::Rc;

use lb_css::{ComputedStyle, Direction, Position, TextAlign};
use lb_dom::{Document, NodeId};

use crate::build::Built;
use crate::engine::{Geo, Rect, creates_stacking_context};
use crate::fonts::TextFont;
use crate::tree::{BoxKind, BoxTree};

/// One line of a text run, ready to draw.
#[derive(Clone, Debug, PartialEq)]
pub struct TextLine {
    pub text: String,
    /// Left edge of the line's text, page coordinates.
    pub x: f32,
    /// Alphabetic baseline, page coordinates.
    pub baseline: f32,
    pub width: f32,
}

/// What a painted box is.
#[derive(Clone, Debug)]
pub enum DisplayKind {
    /// An element's box: background and borders.
    Element,
    /// A replaced element (img, canvas, form controls, iframe, ...): its
    /// background and borders, plus content the painter asks the host for.
    /// `font` is the element's font, for form control text.
    Replaced { font: TextFont },
    /// A text node's run in its parent's style.
    Text {
        font: TextFont,
        lines: Vec<TextLine>,
    },
}

/// One painted box.
#[derive(Clone, Debug)]
pub struct DisplayItem {
    /// Element or text node.
    pub node: NodeId,
    pub kind: DisplayKind,
    /// Border box, page coordinates.
    pub rect: Rect,
    /// Used border widths: top, right, bottom, left.
    pub border: [f32; 4],
    /// Used padding: top, right, bottom, left.
    pub padding: [f32; 4],
    /// Clip from `overflow` ancestors, page coordinates.
    pub clip: Option<Rect>,
    /// Computed style (a text run's: its parent's).
    pub style: Rc<ComputedStyle>,
}

impl DisplayItem {
    /// Padding box.
    pub fn padding_box(&self) -> Rect {
        let [t, r, b, l] = self.border;
        inset(self.rect, t, r, b, l)
    }

    /// Content box.
    pub fn content_box(&self) -> Rect {
        let [t, r, b, l] = self.padding;
        inset(self.padding_box(), t, r, b, l)
    }
}

/// One paint step.
#[derive(Clone, Debug)]
pub enum DisplayStep {
    Item(DisplayItem),
    /// Start of a stacking context painted as a group with this opacity.
    BeginGroup {
        opacity: f32,
    },
    EndGroup,
}

/// Every painted box of a document, in paint order.
#[derive(Clone, Debug, Default)]
pub struct DisplayList {
    pub steps: Vec<DisplayStep>,
    /// Viewport size in CSS px.
    pub viewport: (f32, f32),
    /// Viewport scroll offset (`window.scrollX/Y`).
    pub scroll: (f32, f32),
    /// Scrollable document size (at least the viewport).
    pub document_size: (f32, f32),
    /// The root element and body, whose background paints the canvas.
    pub root: Option<NodeId>,
    pub body: Option<NodeId>,
}

impl DisplayList {
    /// The painted items, ignoring groups.
    pub fn items(&self) -> impl Iterator<Item = &DisplayItem> {
        self.steps.iter().filter_map(|s| match s {
            DisplayStep::Item(i) => Some(i),
            _ => None,
        })
    }
}

fn inset(r: Rect, t: f32, right: f32, b: f32, l: f32) -> Rect {
    Rect::new(
        r.x + l,
        r.y + t,
        (r.width - l - right).max(0.0),
        (r.height - t - b).max(0.0),
    )
}

pub(crate) fn build(
    doc: &Document,
    font_for: &dyn Fn(&ComputedStyle) -> TextFont,
    built: &Built,
    geo: &[Geo],
    viewport: (f32, f32),
    scroll: (f32, f32),
    document_size: (f32, f32),
) -> DisplayList {
    let mut list = DisplayList {
        steps: Vec::with_capacity(built.tree.boxes.len()),
        viewport,
        scroll,
        document_size,
        root: doc.document_element(),
        body: doc.body(),
    };
    let w = Walker {
        font_for,
        tree: &built.tree,
        geo,
        scroll,
    };
    w.context(built.root, &mut list.steps);
    list
}

struct Walker<'a> {
    font_for: &'a dyn Fn(&ComputedStyle) -> TextFont,
    tree: &'a BoxTree,
    geo: &'a [Geo],
    scroll: (f32, f32),
}

impl Walker<'_> {
    fn is_flex_or_grid(&self, i: usize) -> bool {
        self.tree.boxes[i]
            .css
            .as_ref()
            .is_some_and(|c| c.display.is_flex() || c.display.is_grid())
    }

    /// Paints stacking context (or positioned box) `sc` and its contents.
    fn context(&self, sc: usize, out: &mut Vec<DisplayStep>) {
        let opacity = self.tree.boxes[sc].css.as_ref().map_or(1.0, |c| c.opacity);
        let group = opacity < 1.0;
        if group {
            out.push(DisplayStep::BeginGroup {
                opacity: opacity.max(0.0),
            });
        }
        self.item(sc, out);
        let mut neg: Vec<(i32, usize)> = Vec::new();
        let mut normal: Vec<usize> = Vec::new();
        let mut zero: Vec<usize> = Vec::new();
        let mut pos: Vec<(i32, usize)> = Vec::new();
        let mut stack: Vec<usize> = self.tree.boxes[sc]
            .children
            .iter()
            .rev()
            .map(|c| usize::from(*c))
            .collect();
        while let Some(i) = stack.pop() {
            let b = &self.tree.boxes[i];
            let parent_fg = b.parent.is_some_and(|p| self.is_flex_or_grid(p));
            if let Some(css) = &b.css
                && !matches!(b.kind, BoxKind::Text(_))
            {
                if creates_stacking_context(css, parent_fg) {
                    let z = css.z_index.unwrap_or(0);
                    if z < 0 {
                        neg.push((z, i));
                    } else if z == 0 {
                        zero.push(i);
                    } else {
                        pos.push((z, i));
                    }
                    continue;
                }
                if css.position != Position::Static {
                    zero.push(i);
                    continue;
                }
            }
            normal.push(i);
            for c in b.children.iter().rev() {
                stack.push(usize::from(*c));
            }
        }
        neg.sort_by_key(|(z, _)| *z);
        pos.sort_by_key(|(z, _)| *z);
        for (_, i) in neg {
            self.context(i, out);
        }
        for i in normal {
            self.item(i, out);
        }
        for i in zero {
            self.context(i, out);
        }
        for (_, i) in pos {
            self.context(i, out);
        }
        if group {
            out.push(DisplayStep::EndGroup);
        }
    }

    fn item(&self, i: usize, out: &mut Vec<DisplayStep>) {
        let b = &self.tree.boxes[i];
        let (Some(node), Some(style)) = (b.dom, b.css.clone()) else {
            return;
        };
        let g = &self.geo[i];
        let (sx, sy) = self.scroll;
        let rect = g.rect.translate(sx, sy);
        let clip = g.clip.map(|c| c.translate(sx, sy));
        let l = &b.layout;
        let border = [l.border.top, l.border.right, l.border.bottom, l.border.left];
        let padding = [
            l.padding.top,
            l.padding.right,
            l.padding.bottom,
            l.padding.left,
        ];
        let kind = match &b.kind {
            BoxKind::Viewport | BoxKind::AnonymousInline => return,
            BoxKind::Element => DisplayKind::Element,
            BoxKind::Replaced { .. } => DisplayKind::Replaced {
                font: (self.font_for)(&style),
            },
            BoxKind::Text(run) => {
                let Some(font) = run.font().cloned() else {
                    return;
                };
                let lm = font.line_metrics();
                let lh = run.line_height;
                let half_leading = (lh - (lm.ascent + lm.descent)) / 2.0;
                let factor = align_factor(style.text_align, style.direction);
                let lines = run
                    .lines(Some(rect.width))
                    .into_iter()
                    .enumerate()
                    .map(|(n, (text, width))| TextLine {
                        x: rect.x + (rect.width - width).max(0.0) * factor,
                        baseline: rect.y + n as f32 * lh + half_leading + lm.ascent,
                        text,
                        width,
                    })
                    .collect();
                DisplayKind::Text { font, lines }
            }
        };
        out.push(DisplayStep::Item(DisplayItem {
            node,
            kind,
            rect,
            border,
            padding,
            clip,
            style,
        }));
    }
}

/// Where a line sits in its box: 0 = left, 0.5 = centered, 1 = right.
fn align_factor(a: TextAlign, dir: Direction) -> f32 {
    let rtl = dir == Direction::Rtl;
    match a {
        TextAlign::Center | TextAlign::WebkitCenter | TextAlign::InternalCenter => 0.5,
        TextAlign::Right | TextAlign::WebkitRight => 1.0,
        TextAlign::Left | TextAlign::WebkitLeft => 0.0,
        TextAlign::End => {
            if rtl {
                0.0
            } else {
                1.0
            }
        }
        _ => {
            if rtl {
                1.0
            } else {
                0.0
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::LayoutEngine;

    fn engine(html: &str) -> (LayoutEngine, Document) {
        let doc = Document::parse_html(html);
        let e = LayoutEngine::with_env(lb_css::MediaEnv {
            viewport_width: 800.0,
            viewport_height: 600.0,
            ..lb_css::MediaEnv::default()
        });
        (e, doc)
    }

    #[test]
    fn paint_order_groups_and_text() {
        let (mut e, doc) = engine(
            "<body style='margin:0'><div id=a style='height:50px;background:red'></div>\
             <div style='opacity:0.5;position:relative'><p style='margin:0'>Hello world</p></div>\
             <div style='position:absolute;top:0;z-index:-1;width:10px;height:10px'></div></body>",
        );
        let list = e.display_list(&doc);
        assert_eq!(list.viewport, (800.0, 600.0));
        let begin = list
            .steps
            .iter()
            .position(|s| matches!(s, DisplayStep::BeginGroup { opacity } if (*opacity - 0.5).abs() < 1e-6))
            .expect("opacity group");
        let end = list
            .steps
            .iter()
            .position(|s| matches!(s, DisplayStep::EndGroup))
            .expect("group end");
        assert!(begin < end);
        let text = list.steps[begin..end]
            .iter()
            .find_map(|s| match s {
                DisplayStep::Item(DisplayItem {
                    kind: DisplayKind::Text { lines, .. },
                    ..
                }) => Some(lines.clone()),
                _ => None,
            })
            .expect("text inside the group");
        assert_eq!(text[0].text, "Hello world");
        assert!(text[0].baseline > 50.0 && text[0].baseline < 70.0);
        // The negative z-index box paints before the in-flow red div.
        let neg = list
            .items()
            .position(|i| i.rect.width == 10.0 && i.rect.height == 10.0)
            .unwrap();
        let red = list
            .items()
            .position(|i| i.rect.height == 50.0 && i.style.background_color.r == 255)
            .unwrap();
        assert!(neg < red);
    }

    #[test]
    fn scroll_moves_into_page_coordinates() {
        let (mut e, doc) = engine(
            "<body style='margin:0'><div style='height:3000px'></div>\
             <div style='position:fixed;top:0;height:20px;width:20px'></div></body>",
        );
        let html = doc.document_element().unwrap();
        e.scroll_window_to(&doc, 0.0, 500.0);
        let list = e.display_list(&doc);
        assert_eq!(list.scroll, (0.0, 500.0));
        assert_eq!(list.root, Some(html));
        let tall = list.items().find(|i| i.rect.height == 3000.0).unwrap();
        assert_eq!(tall.rect.y, 0.0);
        let fixed = list.items().find(|i| i.rect.height == 20.0).unwrap();
        assert_eq!(fixed.rect.y, 500.0);
    }
}
