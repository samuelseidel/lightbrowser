//! Builds the box tree from the DOM and computed styles.
//!
//! Inline content is approximated: a run of inline-level children becomes
//! an anonymous flex row that wraps (`flex-flow: row wrap`), whose items are
//! text runs, inline elements (themselves wrapping rows) and atomic inlines.
//! Text inside an item wraps at spaces. This keeps inline elements
//! addressable (links, labels, buttons get boxes) at the cost of exact line
//! fragmentation.

use std::collections::HashMap;
use std::rc::Rc;

use lb_css::{
    Align, BoxSizing, ComputedStyle, Display, Float, LineHeight, Lp, LpAuto, Overflow, Position,
    StyleEngine, TextAlign, TrackListEntry, TrackSize,
};
use lb_dom::{Document, NodeId, NodeType, ns};
use taffy::style::{
    AlignContent, AlignItems, Dimension, GridPlacement, GridTemplateComponent, LengthPercentage,
    LengthPercentageAuto, MaxTrackSizingFunction, MinTrackSizingFunction, RepetitionCount,
    TrackSizingFunction,
};
use taffy::{Point, Rect, Size, Style};

use crate::fonts::{FontAvailability, FontDb, TextFont, is_emoji, resolve_font};
use crate::text::{TextRun, collapse};
use crate::tree::{BoxKind, BoxTree, LBox};

/// Output of a build.
pub(crate) struct Built {
    pub(crate) tree: BoxTree,
    pub(crate) root: usize,
    /// Principal box of each rendered element / text node.
    pub(crate) node_box: HashMap<NodeId, usize>,
}

pub(crate) struct Builder<'a> {
    doc: &'a Document,
    styles: &'a mut StyleEngine,
    fonts: &'a FontDb,
    avail: &'a FontAvailability,
    /// Classic scrollbar width in px (0 for overlay scrollbars), from the
    /// persona (`MediaEnv::scrollbar_width`).
    scrollbar: f32,
    tree: BoxTree,
    node_box: HashMap<NodeId, usize>,
    /// The inline content so far ends in a collapsible space (or a run is
    /// just starting), so a leading space is dropped.
    ends_in_space: bool,
    /// Natural sizes of loaded images.
    intrinsic: Option<&'a HashMap<NodeId, (f32, f32)>>,
}

#[derive(Copy, Clone, PartialEq, Eq)]
enum Mode {
    /// Block container: inline-level children are grouped into runs.
    Block,
    /// Flex or grid container: every child is an item.
    Item,
    /// Inside an inline run: children are run items.
    Inline,
}

struct Item {
    idx: usize,
    inline: bool,
}

impl<'a> Builder<'a> {
    pub(crate) fn new(
        doc: &'a Document,
        styles: &'a mut StyleEngine,
        fonts: &'a FontDb,
        avail: &'a FontAvailability,
    ) -> Self {
        let scrollbar = styles.env().scrollbar_width;
        Builder {
            doc,
            styles,
            fonts,
            avail,
            scrollbar,
            tree: BoxTree::default(),
            node_box: HashMap::new(),
            ends_in_space: true,
            intrinsic: None,
        }
    }

    /// Natural sizes of loaded images (see `LayoutEngine::set_intrinsic_size`).
    pub(crate) fn with_intrinsic(mut self, map: &'a HashMap<NodeId, (f32, f32)>) -> Self {
        self.intrinsic = Some(map);
        self
    }

    pub(crate) fn build(mut self, width: f32, height: f32) -> Built {
        let vp = Style {
            display: taffy::Display::Block,
            size: Size {
                width: Dimension::length(width),
                height: Dimension::length(height),
            },
            ..Style::DEFAULT
        };
        let root = self.tree.push(LBox::new(None, BoxKind::Viewport, vp));
        if let Some(html) = self.doc.document_element() {
            for it in self.element(html, Mode::Block) {
                self.tree.append(root, it.idx);
            }
        }
        // The box list grew by doubling and lives as long as the layout.
        self.tree.boxes.shrink_to_fit();
        Built {
            tree: self.tree,
            root,
            node_box: self.node_box,
        }
    }

    fn style_of(&mut self, el: NodeId) -> Option<Rc<ComputedStyle>> {
        self.styles.computed_style(self.doc, el)
    }

    /// Boxes for an element (none for `display: none`, the children's for
    /// `display: contents`).
    fn element(&mut self, el: NodeId, parent_mode: Mode) -> Vec<Item> {
        let Some(css) = self.style_of(el) else {
            return Vec::new();
        };
        match css.display {
            Display::None => return Vec::new(),
            Display::Contents => return self.children(el, &css, parent_mode),
            _ => {}
        }
        let out_of_flow = matches!(css.position, Position::Absolute | Position::Fixed);
        let inline = parent_mode != Mode::Item
            && css.display.is_inline_level()
            && !out_of_flow
            && css.float == Float::None;
        let local = self
            .doc
            .element(el)
            .filter(|e| e.name.ns == ns!(html))
            .map(|e| e.name.local.to_string())
            .unwrap_or_default();
        let in_run = parent_mode == Mode::Inline;
        if let Some(kind) = self.replaced(el, &local, &css) {
            // Replaced elements are atomic: sizes apply even when inline.
            let mut style = taffy_style(&css, &mut self.tree, in_run, true);
            style.scrollbar_width = self.scrollbar;
            style.item_is_replaced = local != "input" || kind.1;
            style.item_is_compressible_replaced = matches!(
                local.as_str(),
                "input" | "select" | "textarea" | "meter" | "progress"
            );
            if inline {
                style.flex_shrink = 0.0;
            }
            if let BoxKind::Replaced { ratio: Some(r), .. } = &kind.0 {
                style.aspect_ratio = Some(*r);
            }
            let mut b = LBox::new(Some(el), kind.0, style);
            b.css = Some(css);
            let idx = self.tree.push(b);
            self.node_box.insert(el, idx);
            self.ends_in_space = !inline;
            return vec![Item { idx, inline }];
        }
        let mut style = taffy_style(&css, &mut self.tree, in_run, false);
        style.scrollbar_width = self.scrollbar;
        if local == "br" {
            // A forced break: a zero-height item that takes a whole line.
            style.flex_basis = Dimension::percent(1.0);
            style.size.height = Dimension::length(0.0);
            let mut b = LBox::new(Some(el), BoxKind::Element, style);
            b.css = Some(css);
            let idx = self.tree.push(b);
            self.node_box.insert(el, idx);
            self.ends_in_space = true;
            return vec![Item { idx, inline: true }];
        }
        let is_inline_box = css.display == Display::Inline && inline;
        if inline && !is_inline_box {
            // Atomic inline: does not shrink in the run.
            style.flex_shrink = 0.0;
        }
        if parent_mode == Mode::Inline && !inline {
            // Block inside inline: takes its own line.
            style.flex_basis = Dimension::percent(1.0);
        }
        let mut b = LBox::new(Some(el), BoxKind::Element, style);
        b.css = Some(css.clone());
        let idx = self.tree.push(b);
        self.node_box.insert(el, idx);
        let mode = if is_inline_box {
            Mode::Inline
        } else if css.display.is_flex() || css.display.is_grid() || is_flexish_table(css.display) {
            Mode::Item
        } else {
            Mode::Block
        };
        if mode == Mode::Block {
            self.ends_in_space = true;
        }
        let items = self.children(el, &css, mode);
        self.attach(idx, items, mode, &css);
        if !is_inline_box {
            self.ends_in_space = !inline;
        }
        vec![Item { idx, inline }]
    }

    /// Appends items to `container`, grouping inline runs in block mode.
    fn attach(&mut self, container: usize, items: Vec<Item>, mode: Mode, css: &ComputedStyle) {
        if mode != Mode::Block {
            for it in items {
                self.tree.append(container, it.idx);
            }
            return;
        }
        let mut run: Vec<usize> = Vec::new();
        let flush = |this: &mut Self, run: &mut Vec<usize>| {
            if run.is_empty() {
                return;
            }
            trim_run_end(&mut this.tree, run);
            if run.is_empty() {
                return;
            }
            let anon = this.tree.push(LBox::new(
                None,
                BoxKind::AnonymousInline,
                inline_run_style(css),
            ));
            for &i in run.iter() {
                this.tree.append(anon, i);
            }
            this.tree.append(container, anon);
            run.clear();
        };
        for it in items {
            if it.inline {
                run.push(it.idx);
            } else {
                flush(self, &mut run);
                self.tree.append(container, it.idx);
            }
        }
        flush(self, &mut run);
    }

    /// Items for the children of `el` (`css` is its style, for text).
    fn children(&mut self, el: NodeId, css: &Rc<ComputedStyle>, mode: Mode) -> Vec<Item> {
        let mut items = Vec::new();
        let kids: Vec<NodeId> = self.doc.flat_children(el);
        for c in kids {
            match self.doc.node_type(c) {
                NodeType::Text => {
                    if let Some(idx) = self.text(c, css, mode) {
                        items.push(Item {
                            idx,
                            inline: mode != Mode::Item,
                        });
                    }
                }
                NodeType::Element => {
                    let child_items = self.element(c, mode);
                    items.extend(child_items);
                }
                _ => {}
            }
        }
        items
    }

    fn text(&mut self, node: NodeId, css: &Rc<ComputedStyle>, mode: Mode) -> Option<usize> {
        let raw = self.doc.character_data(node)?;
        let ws = css.white_space;
        let strip = self.ends_in_space || mode == Mode::Item;
        let text = collapse(raw, ws, strip && ws.collapses_spaces());
        if text.is_empty() || (mode == Mode::Item && text.trim().is_empty()) {
            return None;
        }
        if ws.collapses_spaces() {
            self.ends_in_space = text.ends_with(' ');
        } else {
            self.ends_in_space = false;
        }
        let font = resolve_font(self.fonts, self.avail, css);
        let mut lh = font.line_height(css);
        // `line-height: normal` grows to fit the emoji fallback font.
        if css.line_height == LineHeight::Normal
            && text.chars().any(is_emoji)
            && let Some(e) = self.avail.emoji_family(self.fonts)
        {
            lh = lh.max(e.line_metrics(css.font_size).normal_line_height());
        }
        let run = TextRun::new(text, &font, lh, ws);
        let style = Style {
            display: taffy::Display::Block,
            flex_shrink: 1.0,
            ..Style::DEFAULT
        };
        let mut b = LBox::new(Some(node), BoxKind::Text(run), style);
        b.css = Some(css.clone());
        let idx = self.tree.push(b);
        self.node_box.insert(node, idx);
        Some(idx)
    }

    /// Replaced-element intrinsic size (content box), or `None` if not
    /// replaced. The bool is true for inputs that behave like images.
    fn replaced(
        &mut self,
        el: NodeId,
        local: &str,
        css: &ComputedStyle,
    ) -> Option<(BoxKind, bool)> {
        let attr = |n: &str| self.doc.get_attribute(el, n);
        let num = |n: &str| attr(n).and_then(|v| v.trim().parse::<f32>().ok());
        let font = || resolve_font(self.fonts, self.avail, css);
        let k = css.font_size * 3.0 / 40.0;
        let sized = |w: f32, h: f32| BoxKind::Replaced {
            width: Some(w),
            height: Some(h),
            ratio: None,
        };
        // Blink sizes a textarea for its scrollbar even when none shows.
        let scrollbar = self.scrollbar;
        Some(match local {
            "img" if self.intrinsic.and_then(|m| m.get(&el)).is_some() => {
                let (w, h) = self
                    .intrinsic
                    .and_then(|m| m.get(&el))
                    .copied()
                    .unwrap_or_default();
                (
                    BoxKind::Replaced {
                        width: Some(w),
                        height: Some(h),
                        ratio: (h > 0.0).then_some(w / h),
                    },
                    true,
                )
            }
            "img" => {
                let (w, h) = (num("width"), num("height"));
                (
                    BoxKind::Replaced {
                        width: None,
                        height: None,
                        ratio: w.zip(h).filter(|(_, h)| *h > 0.0).map(|(w, h)| w / h),
                    },
                    true,
                )
            }
            "canvas" => (
                sized(
                    num("width").unwrap_or(300.0),
                    num("height").unwrap_or(150.0),
                ),
                true,
            ),
            "video" | "iframe" | "embed" | "object" | "svg" => (sized(300.0, 150.0), true),
            "meter" => (sized(80.0, 16.0), false),
            "progress" => (sized(160.0, 16.0), false),
            "textarea" => {
                let f = font();
                let cols = num("cols").filter(|c| *c > 0.0).unwrap_or(20.0);
                let rows = num("rows").filter(|r| *r > 0.0).unwrap_or(2.0);
                let avg = f.measure("0");
                (
                    sized(cols * avg + scrollbar, rows * f.line_height(css)),
                    false,
                )
            }
            "select" => {
                let f = font();
                let widest = self
                    .doc
                    .descendants(el)
                    .filter(|&n| {
                        self.doc
                            .is_html_element_named(n, &lb_dom::local_name!("option"))
                    })
                    .map(|n| f.measure(self.doc.text_content(n).unwrap_or_default().trim()))
                    .fold(0.0, f32::max);
                let lh = f.line_height(css);
                let multi = attr("multiple").is_some() || num("size").is_some_and(|s| s > 1.0);
                let rows = if multi {
                    num("size").unwrap_or(4.0)
                } else {
                    1.0
                };
                (sized(widest + 24.0, rows * lh + 2.0), false)
            }
            "input" => {
                let ty = attr("type").unwrap_or("text").to_ascii_lowercase();
                match ty.as_str() {
                    "checkbox" | "radio" => (sized(13.0, 13.0), false),
                    "range" => (sized(129.0, 16.0), false),
                    "color" => (sized(44.0, 23.0), false),
                    "image" => (
                        BoxKind::Replaced {
                            width: None,
                            height: None,
                            ratio: None,
                        },
                        true,
                    ),
                    "submit" | "reset" | "button" => {
                        let f = font();
                        let label =
                            attr("value")
                                .map(str::to_owned)
                                .unwrap_or_else(|| match ty.as_str() {
                                    "submit" => "Submit".into(),
                                    "reset" => "Reset".into(),
                                    _ => String::new(),
                                });
                        let lh = f.line_height(css);
                        (sized(f.measure(&label), lh), false)
                    }
                    "file" => (sized(253.0 * k, font().line_height(css) + 6.0), false),
                    _ => {
                        let f: TextFont = font();
                        let size = num("size").filter(|s| *s > 0.0).unwrap_or(20.0);
                        (sized(145.0 * k * size / 20.0, f.line_height(css)), false)
                    }
                }
            }
            _ => return None,
        })
    }
}

fn is_flexish_table(d: Display) -> bool {
    d == Display::TableRow
}

/// Removes a trailing collapsible space from the run's last text item.
fn trim_run_end(tree: &mut BoxTree, run: &mut Vec<usize>) {
    while let Some(&last) = run.last() {
        let b = &mut tree.boxes[last];
        if let BoxKind::Text(t) = &mut b.kind {
            let collapses = b
                .css
                .as_ref()
                .is_none_or(|c| c.white_space.collapses_spaces());
            if collapses && t.text.ends_with(' ') {
                let trimmed = t.text.trim_end_matches(' ').to_owned();
                if trimmed.is_empty() {
                    run.pop();
                    continue;
                }
                t.trim_to(trimmed);
            }
        }
        break;
    }
}

fn inline_run_style(container: &ComputedStyle) -> Style {
    let justify = match container.text_align {
        TextAlign::Center | TextAlign::WebkitCenter | TextAlign::InternalCenter => {
            AlignContent::CENTER
        }
        TextAlign::Right | TextAlign::End | TextAlign::WebkitRight => AlignContent::FLEX_END,
        _ => AlignContent::START,
    };
    Style {
        display: taffy::Display::Flex,
        flex_direction: taffy::FlexDirection::Row,
        flex_wrap: if container.white_space.wraps() {
            taffy::FlexWrap::Wrap
        } else {
            taffy::FlexWrap::NoWrap
        },
        align_items: AlignItems::FLEX_START,
        align_content: AlignContent::FLEX_START,
        justify_content: justify,
        ..Style::DEFAULT
    }
}

fn lp(tree: &mut BoxTree, l: &Lp) -> LengthPercentage {
    match l {
        Lp::Px(v) => LengthPercentage::length(*v),
        Lp::Pct(p) => LengthPercentage::percent(p / 100.0),
        Lp::Calc(c) => LengthPercentage::calc(tree.calc_handle(c.clone())),
    }
}

fn lp_auto(tree: &mut BoxTree, l: &LpAuto) -> LengthPercentageAuto {
    match l {
        LpAuto::Auto => LengthPercentageAuto::auto(),
        LpAuto::Lp(Lp::Px(v)) => LengthPercentageAuto::length(*v),
        LpAuto::Lp(Lp::Pct(p)) => LengthPercentageAuto::percent(p / 100.0),
        LpAuto::Lp(Lp::Calc(c)) => LengthPercentageAuto::calc(tree.calc_handle(c.clone())),
    }
}

fn dim(tree: &mut BoxTree, l: &LpAuto) -> Dimension {
    match l {
        LpAuto::Auto => Dimension::auto(),
        LpAuto::Lp(Lp::Px(v)) => Dimension::length(*v),
        LpAuto::Lp(Lp::Pct(p)) => Dimension::percent(p / 100.0),
        LpAuto::Lp(Lp::Calc(c)) => Dimension::calc(tree.calc_handle(c.clone())),
    }
}

fn align_items(a: Align) -> Option<AlignItems> {
    Some(match a {
        Align::Normal | Align::Auto | Align::Legacy => return None,
        Align::Stretch => AlignItems::STRETCH,
        Align::Start | Align::Left => AlignItems::START,
        Align::End | Align::Right => AlignItems::END,
        Align::FlexStart => AlignItems::FLEX_START,
        Align::FlexEnd => AlignItems::FLEX_END,
        Align::Center | Align::AnchorCenter => AlignItems::CENTER,
        Align::Baseline | Align::LastBaseline => AlignItems::BASELINE,
        Align::SelfStart => AlignItems::SELF_START,
        Align::SelfEnd => AlignItems::SELF_END,
        Align::SpaceBetween | Align::SpaceAround | Align::SpaceEvenly => AlignItems::FLEX_START,
    })
}

fn align_content(a: Align) -> AlignContent {
    match a {
        Align::Stretch => AlignContent::STRETCH,
        Align::Start | Align::Left | Align::SelfStart | Align::Baseline => AlignContent::START,
        Align::End | Align::Right | Align::SelfEnd | Align::LastBaseline => AlignContent::END,
        Align::FlexStart => AlignContent::FLEX_START,
        Align::FlexEnd => AlignContent::FLEX_END,
        Align::Center | Align::AnchorCenter => AlignContent::CENTER,
        Align::SpaceBetween => AlignContent::SPACE_BETWEEN,
        Align::SpaceAround => AlignContent::SPACE_AROUND,
        Align::SpaceEvenly => AlignContent::SPACE_EVENLY,
        Align::Normal | Align::Auto | Align::Legacy => AlignContent::NORMAL,
    }
}

fn overflow(o: Overflow) -> taffy::Overflow {
    match o {
        Overflow::Visible => taffy::Overflow::Visible,
        Overflow::Clip => taffy::Overflow::Clip,
        Overflow::Hidden | Overflow::Auto | Overflow::Overlay => taffy::Overflow::Hidden,
        Overflow::Scroll => taffy::Overflow::Scroll,
    }
}

fn track(tree: &mut BoxTree, t: &TrackSize) -> TrackSizingFunction {
    let min = |tree: &mut BoxTree, t: &TrackSize| -> MinTrackSizingFunction {
        match t {
            TrackSize::Lp(l) => MinTrackSizingFunction::from(lp(tree, l)),
            TrackSize::MinContent => MinTrackSizingFunction::min_content(),
            TrackSize::MaxContent => MinTrackSizingFunction::max_content(),
            _ => MinTrackSizingFunction::auto(),
        }
    };
    let max = |tree: &mut BoxTree, t: &TrackSize| -> MaxTrackSizingFunction {
        match t {
            TrackSize::Lp(l) => MaxTrackSizingFunction::from(lp(tree, l)),
            TrackSize::Fr(v) => MaxTrackSizingFunction::fr(*v),
            TrackSize::MinContent => MaxTrackSizingFunction::min_content(),
            TrackSize::MaxContent => MaxTrackSizingFunction::max_content(),
            TrackSize::FitContent(Lp::Px(v)) => MaxTrackSizingFunction::fit_content_px(*v),
            TrackSize::FitContent(Lp::Pct(p)) => {
                MaxTrackSizingFunction::fit_content_percent(p / 100.0)
            }
            TrackSize::FitContent(Lp::Calc(c)) => {
                MaxTrackSizingFunction::fit_content_calc(tree.calc_handle(c.clone()))
            }
            _ => MaxTrackSizingFunction::auto(),
        }
    };
    match t {
        TrackSize::MinMax(a, b) => TrackSizingFunction {
            min: min(tree, a),
            max: max(tree, b),
        },
        TrackSize::Fr(_) => TrackSizingFunction {
            min: MinTrackSizingFunction::auto(),
            max: max(tree, t),
        },
        other => TrackSizingFunction {
            min: min(tree, other),
            max: max(tree, other),
        },
    }
}

fn tracks(tree: &mut BoxTree, list: &[TrackListEntry]) -> Vec<GridTemplateComponent<String>> {
    list.iter()
        .map(|e| match e {
            TrackListEntry::Track(t) => GridTemplateComponent::Single(track(tree, t)),
            TrackListEntry::Repeat(n, ts) => {
                let count = match n {
                    0 => RepetitionCount::AutoFill,
                    -1 => RepetitionCount::AutoFit,
                    n => RepetitionCount::Count(*n as u16),
                };
                let ts: Vec<TrackSizingFunction> = ts.iter().map(|t| track(tree, t)).collect();
                taffy::style_helpers::repeat(count, ts)
            }
        })
        .collect()
}

fn grid_line(g: &lb_css::GridLine) -> GridPlacement<String> {
    match g {
        lb_css::GridLine::Auto => GridPlacement::Auto,
        lb_css::GridLine::Line(n) => taffy::style_helpers::line(*n as i16),
        lb_css::GridLine::Span(n) => GridPlacement::Span(*n),
        lb_css::GridLine::Named(_) => GridPlacement::Auto,
    }
}

/// Maps a computed style to taffy's style. `in_inline_run` zeroes the
/// vertical margins of inline boxes (they do not affect line layout);
/// `atomic` treats an inline element as an atomic inline (replaced
/// elements), so its sizes apply.
pub(crate) fn taffy_style(
    css: &ComputedStyle,
    tree: &mut BoxTree,
    in_inline_run: bool,
    atomic: bool,
) -> Style {
    let out_of_flow = matches!(css.position, Position::Absolute | Position::Fixed);
    let bfc = css.float != Float::None
        || out_of_flow
        || css.is_scroll_container()
        || matches!(
            css.display,
            Display::FlowRoot
                | Display::InlineBlock
                | Display::Table
                | Display::InlineTable
                | Display::TableCell
                | Display::TableCaption
        );
    let is_inline = css.display == Display::Inline && !atomic;
    let display = if css.display.is_flex() || css.display == Display::TableRow || is_inline {
        taffy::Display::Flex
    } else if css.display.is_grid() {
        taffy::Display::Grid
    } else if bfc {
        taffy::Display::FlowRoot
    } else {
        taffy::Display::Block
    };
    let mut s = Style {
        display,
        box_sizing: match css.box_sizing {
            BoxSizing::BorderBox => taffy::BoxSizing::BorderBox,
            BoxSizing::ContentBox => taffy::BoxSizing::ContentBox,
        },
        overflow: Point {
            x: overflow(css.overflow_x),
            y: overflow(css.overflow_y),
        },
        scrollbar_width: 0.0,
        position: match css.position {
            Position::Static => taffy::Position::Static,
            Position::Relative => taffy::Position::Relative,
            Position::Absolute => taffy::Position::Absolute,
            Position::Fixed => taffy::Position::Fixed,
            Position::Sticky => taffy::Position::Sticky,
        },
        float: match css.float {
            Float::None => taffy::Float::None,
            Float::Left | Float::InlineStart => taffy::Float::Left,
            Float::Right | Float::InlineEnd => taffy::Float::Right,
        },
        clear: match css.clear {
            lb_css::Clear::None => taffy::Clear::None,
            lb_css::Clear::Left | lb_css::Clear::InlineStart => taffy::Clear::Left,
            lb_css::Clear::Right | lb_css::Clear::InlineEnd => taffy::Clear::Right,
            lb_css::Clear::Both => taffy::Clear::Both,
        },
        ..Style::DEFAULT
    };
    s.inset = Rect {
        top: lp_auto(tree, &css.inset[0]),
        right: lp_auto(tree, &css.inset[1]),
        bottom: lp_auto(tree, &css.inset[2]),
        left: lp_auto(tree, &css.inset[3]),
    };
    if !is_inline {
        s.size = Size {
            width: dim(tree, &css.width),
            height: dim(tree, &css.height),
        };
        s.min_size = Size {
            width: lp_auto(tree, &css.min_width),
            height: lp_auto(tree, &css.min_height),
        };
        s.max_size = Size {
            width: css
                .max_width
                .as_ref()
                .map_or(LengthPercentageAuto::auto(), |l| {
                    lp_auto(tree, &LpAuto::Lp(l.clone()))
                }),
            height: css
                .max_height
                .as_ref()
                .map_or(LengthPercentageAuto::auto(), |l| {
                    lp_auto(tree, &LpAuto::Lp(l.clone()))
                }),
        };
        s.aspect_ratio = css.aspect_ratio;
    }
    let zero_vertical = is_inline && in_inline_run;
    s.margin = Rect {
        top: if zero_vertical {
            LengthPercentageAuto::length(0.0)
        } else {
            lp_auto(tree, &css.margin[0])
        },
        right: lp_auto(tree, &css.margin[1]),
        bottom: if zero_vertical {
            LengthPercentageAuto::length(0.0)
        } else {
            lp_auto(tree, &css.margin[2])
        },
        left: lp_auto(tree, &css.margin[3]),
    };
    s.padding = Rect {
        top: lp(tree, &css.padding[0]),
        right: lp(tree, &css.padding[1]),
        bottom: lp(tree, &css.padding[2]),
        left: lp(tree, &css.padding[3]),
    };
    s.border = Rect {
        top: LengthPercentage::length(css.border_width[0]),
        right: LengthPercentage::length(css.border_width[1]),
        bottom: LengthPercentage::length(css.border_width[2]),
        left: LengthPercentage::length(css.border_width[3]),
    };
    if is_inline {
        s.flex_direction = taffy::FlexDirection::Row;
        s.flex_wrap = if css.white_space.wraps() {
            taffy::FlexWrap::Wrap
        } else {
            taffy::FlexWrap::NoWrap
        };
        s.align_items = AlignItems::FLEX_START;
        s.flex_shrink = 1.0;
        return s;
    }
    s.flex_direction = match css.flex_direction {
        lb_css::FlexDirection::Row => taffy::FlexDirection::Row,
        lb_css::FlexDirection::RowReverse => taffy::FlexDirection::RowReverse,
        lb_css::FlexDirection::Column => taffy::FlexDirection::Column,
        lb_css::FlexDirection::ColumnReverse => taffy::FlexDirection::ColumnReverse,
    };
    s.flex_wrap = match css.flex_wrap {
        lb_css::FlexWrap::Nowrap => taffy::FlexWrap::NoWrap,
        lb_css::FlexWrap::Wrap => taffy::FlexWrap::Wrap,
        lb_css::FlexWrap::WrapReverse => taffy::FlexWrap::WrapReverse,
    };
    s.flex_grow = css.flex_grow;
    s.flex_shrink = css.flex_shrink;
    s.flex_basis = dim(tree, &css.flex_basis);
    if css.display == Display::TableCell {
        // Cells share a row (approximated as a flex row).
        s.flex_grow = 1.0;
    }
    s.gap = Size {
        width: css
            .column_gap
            .as_ref()
            .map_or(LengthPercentage::length(0.0), |l| lp(tree, l)),
        height: css
            .row_gap
            .as_ref()
            .map_or(LengthPercentage::length(0.0), |l| lp(tree, l)),
    };
    s.align_items = align_items(css.align_items).unwrap_or(AlignItems::NORMAL);
    s.align_self = align_items(css.align_self);
    s.align_content = align_content(css.align_content);
    s.justify_content = align_content(css.justify_content);
    s.justify_items = align_items(css.justify_items).unwrap_or(AlignItems::NORMAL);
    s.justify_self = align_items(css.justify_self);
    if let Some(t) = &css.grid_template_columns {
        s.grid_template_columns = tracks(tree, t);
    }
    if let Some(t) = &css.grid_template_rows {
        s.grid_template_rows = tracks(tree, t);
    }
    s.grid_row = taffy::Line {
        start: grid_line(&css.grid_row_start),
        end: grid_line(&css.grid_row_end),
    };
    s.grid_column = taffy::Line {
        start: grid_line(&css.grid_column_start),
        end: grid_line(&css.grid_column_end),
    };
    s.text_align = match css.text_align {
        TextAlign::WebkitCenter => taffy::TextAlign::LegacyCenter,
        TextAlign::WebkitLeft => taffy::TextAlign::LegacyLeft,
        TextAlign::WebkitRight => taffy::TextAlign::LegacyRight,
        _ => taffy::TextAlign::Auto,
    };
    s
}
