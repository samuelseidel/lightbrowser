//! The box tree handed to taffy: one box per rendered element, text run and
//! anonymous inline run, with taffy's layout traits implemented over it.

use std::rc::Rc;

use lb_css::{Calc, ComputedStyle};
use lb_dom::NodeId;
use taffy::{
    AvailableSpace, Cache, Layout, LayoutBlockContainer, LayoutContainingBlock,
    LayoutFlexboxContainer, LayoutGridContainer, LayoutInput, LayoutOutput, LayoutPartialTree,
    NodeId as TNode, RunMode, Size, Style, TraversePartialTree, TraverseTree, compute_block_layout,
    compute_cached_layout, compute_flexbox_layout, compute_grid_layout, compute_hidden_layout,
    compute_leaf_layout, compute_oof_layout,
};

use crate::text::TextRun;

/// What a box represents.
#[derive(Debug)]
pub(crate) enum BoxKind {
    /// The initial containing block (viewport-sized).
    Viewport,
    /// An element's principal box.
    Element,
    /// Anonymous wrapper for a run of inline-level content.
    AnonymousInline,
    /// A text node's run.
    Text(TextRun),
    /// A replaced element (img, input, ...) with intrinsic sizes.
    Replaced {
        width: Option<f32>,
        height: Option<f32>,
        ratio: Option<f32>,
    },
}

#[derive(Debug)]
pub(crate) struct LBox {
    /// DOM node (element or text) this box belongs to.
    pub(crate) dom: Option<NodeId>,
    pub(crate) kind: BoxKind,
    /// Shared between boxes with equal styles (see [`BoxTree::push`]);
    /// changed in place with `Rc::make_mut`.
    pub(crate) style: Rc<Style>,
    /// Computed style of the element (text boxes: their parent's).
    pub(crate) css: Option<Rc<ComputedStyle>>,
    pub(crate) children: Vec<TNode>,
    pub(crate) parent: Option<usize>,
    cache: Cache,
    pub(crate) layout: Layout,
    hoisted: Vec<TNode>,
}

impl LBox {
    pub(crate) fn new(dom: Option<NodeId>, kind: BoxKind, style: Style) -> LBox {
        LBox {
            dom,
            kind,
            style: Rc::new(style),
            css: None,
            children: Vec::new(),
            parent: None,
            cache: Cache::new(),
            layout: Layout::new(),
            hoisted: Vec::new(),
        }
    }

    pub(crate) fn is_out_of_flow(&self) -> bool {
        self.style.position.is_out_of_flow() && self.style.display != taffy::Display::None
    }
}

/// The box arena plus the `calc()` table taffy resolves through.
#[derive(Debug, Default)]
pub(crate) struct BoxTree {
    pub(crate) boxes: Vec<LBox>,
    calcs: Vec<Rc<Calc>>,
    /// The last few distinct styles pushed, most recent first.
    recent_styles: Vec<Rc<Style>>,
}

/// How many recent styles [`BoxTree::push`] compares a new box's with.
const RECENT_STYLES: usize = 8;

impl BoxTree {
    /// Adds a box. A taffy `Style` is 552 bytes and most boxes share one
    /// with a recent box (every text run has the same, so do the rows and
    /// cells of a table), so an equal style is shared instead of stored
    /// again.
    pub(crate) fn push(&mut self, mut b: LBox) -> usize {
        if let Some(at) = self.recent_styles.iter().position(|s| *s == b.style) {
            let s = self.recent_styles.remove(at);
            b.style = s.clone();
            self.recent_styles.insert(0, s);
        } else {
            self.recent_styles.truncate(RECENT_STYLES - 1);
            self.recent_styles.insert(0, b.style.clone());
        }
        self.boxes.push(b);
        self.boxes.len() - 1
    }

    pub(crate) fn append(&mut self, parent: usize, child: usize) {
        self.boxes[child].parent = Some(parent);
        self.boxes[parent].children.push(TNode::from(child));
    }

    /// Registers a calc expression; returns taffy's opaque handle. The
    /// handle is an index shifted past taffy's 3 tag bits, never a real
    /// pointer, so no `unsafe` is needed to read it back.
    pub(crate) fn calc_handle(&mut self, c: Rc<Calc>) -> *const () {
        self.calcs.push(c);
        std::ptr::without_provenance(self.calcs.len() << 3)
    }

    fn resolve_calc(&self, handle: *const (), basis: f32) -> f32 {
        let idx = (handle.addr() >> 3).wrapping_sub(1);
        self.calcs.get(idx).map_or(0.0, |c| c.eval(basis))
    }

    pub(crate) fn hoisted(&self, i: usize) -> &[TNode] {
        &self.boxes[i].hoisted
    }

    /// Forgets every cached taffy result (styles changed in place).
    pub(crate) fn clear_caches(&mut self) {
        for b in &mut self.boxes {
            b.cache.clear();
        }
    }

    /// Runs taffy from the viewport box.
    pub(crate) fn compute(&mut self, root: usize, width: f32, height: f32) {
        taffy::compute_root_layout(
            self,
            TNode::from(root),
            Size {
                width: AvailableSpace::Definite(width),
                height: AvailableSpace::Definite(height),
            },
        );
    }

    fn node(&self, id: TNode) -> &LBox {
        &self.boxes[usize::from(id)]
    }

    fn node_mut(&mut self, id: TNode) -> &mut LBox {
        &mut self.boxes[usize::from(id)]
    }

    fn compute_child(
        &mut self,
        node_id: TNode,
        inputs: LayoutInput,
        block_ctx: Option<&mut taffy::BlockContext<'_>>,
    ) -> LayoutOutput {
        if inputs.run_mode == RunMode::PerformHiddenLayout {
            self.node_mut(node_id).layout = Layout::with_order(0);
            return compute_hidden_layout(self, node_id);
        }
        compute_cached_layout(self, node_id, inputs, |tree, node_id, inputs| {
            let display = tree.node(node_id).style.display;
            let has_children = !tree.node(node_id).children.is_empty();
            let mut output = match (display, has_children) {
                (taffy::Display::None, _) => compute_hidden_layout(tree, node_id),
                (taffy::Display::Block, true) => {
                    compute_block_layout(tree, node_id, inputs, block_ctx)
                }
                (taffy::Display::FlowRoot, true) => {
                    compute_block_layout(tree, node_id, inputs, None)
                }
                (taffy::Display::Flex, true) => compute_flexbox_layout(tree, node_id, inputs),
                (taffy::Display::Grid, true) => compute_grid_layout(tree, node_id, inputs),
                (_, false) => tree.measure_leaf(node_id, inputs),
            };
            if inputs.run_mode == RunMode::PerformLayout {
                compute_oof_layout(tree, node_id, &mut output);
            }
            output
        })
    }

    fn measure_leaf(&self, node_id: TNode, inputs: LayoutInput) -> LayoutOutput {
        let b = self.node(node_id);
        let calc = |h: *const (), basis: f32| self.resolve_calc(h, basis);
        match &b.kind {
            BoxKind::Text(run) => compute_leaf_layout(inputs, &*b.style, calc, |known, avail| {
                let max = known.width.or(match avail.width {
                    AvailableSpace::Definite(w) => Some(w),
                    AvailableSpace::MinContent => Some(0.0),
                    AvailableSpace::MaxContent => None,
                });
                let (w, h) = if avail.width == AvailableSpace::MinContent && known.width.is_none() {
                    let (_, h) = run.size(Some(run.min_content()));
                    (run.min_content(), h)
                } else {
                    run.size(max)
                };
                Size {
                    width: known.width.unwrap_or(w),
                    height: known.height.unwrap_or(h),
                }
            }),
            BoxKind::Replaced {
                width,
                height,
                ratio,
            } => compute_leaf_layout(inputs, &*b.style, calc, |known, _| {
                let (w, h) = match (known.width, known.height) {
                    (Some(w), Some(h)) => (w, h),
                    (Some(w), None) => (w, ratio.map_or(height.unwrap_or(0.0), |r| w / r)),
                    (None, Some(h)) => (ratio.map_or(width.unwrap_or(0.0), |r| h * r), h),
                    (None, None) => (width.unwrap_or(0.0), height.unwrap_or(0.0)),
                };
                Size {
                    width: w,
                    height: h,
                }
            }),
            _ => compute_leaf_layout(inputs, &*b.style, calc, |known, _| Size {
                width: known.width.unwrap_or(0.0),
                height: known.height.unwrap_or(0.0),
            }),
        }
    }
}

/// Child id iterator.
pub(crate) type ChildIter<'a> = std::iter::Copied<std::slice::Iter<'a, TNode>>;

impl TraversePartialTree for BoxTree {
    type ChildIter<'a> = ChildIter<'a>;

    fn child_ids(&self, parent: TNode) -> Self::ChildIter<'_> {
        self.node(parent).children.iter().copied()
    }

    fn child_count(&self, parent: TNode) -> usize {
        self.node(parent).children.len()
    }

    fn get_child_id(&self, parent: TNode, index: usize) -> TNode {
        self.node(parent).children[index]
    }
}

impl TraverseTree for BoxTree {}

impl LayoutPartialTree for BoxTree {
    type CoreContainerStyle<'a> = &'a Style;
    type CustomIdent = String;

    fn get_core_container_style(&self, node_id: TNode) -> Self::CoreContainerStyle<'_> {
        &self.node(node_id).style
    }

    fn resolve_calc_value(&self, val: *const (), basis: f32) -> f32 {
        self.resolve_calc(val, basis)
    }

    fn set_unrounded_layout(&mut self, node_id: TNode, layout: &Layout) {
        self.node_mut(node_id).layout = *layout;
    }

    fn compute_child_layout(&mut self, node_id: TNode, inputs: LayoutInput) -> LayoutOutput {
        self.compute_child(node_id, inputs, None)
    }
}

impl LayoutContainingBlock for BoxTree {
    type OofItemStyle<'a> = &'a Style;

    fn get_oof_item_style(&self, node_id: TNode) -> Self::OofItemStyle<'_> {
        &self.node(node_id).style
    }

    fn clear_hoisted_children(&mut self, node_id: TNode) {
        self.node_mut(node_id).hoisted.clear();
    }

    fn add_hoisted_children(&mut self, node_id: TNode, hoisted: &[TNode]) {
        self.node_mut(node_id).hoisted.extend_from_slice(hoisted);
    }
}

impl taffy::CacheTree for BoxTree {
    fn cache_get(&mut self, node_id: TNode, input: &LayoutInput) -> Option<LayoutOutput> {
        self.node_mut(node_id).cache.get(input)
    }

    fn cache_store(&mut self, node_id: TNode, input: &LayoutInput, output: LayoutOutput) {
        self.node_mut(node_id).cache.store(input, output)
    }

    fn cache_clear(&mut self, node_id: TNode) {
        self.node_mut(node_id).cache.clear();
    }
}

impl LayoutBlockContainer for BoxTree {
    type BlockContainerStyle<'a> = &'a Style;
    type BlockItemStyle<'a> = &'a Style;

    fn get_block_container_style(&self, node_id: TNode) -> Self::BlockContainerStyle<'_> {
        &self.node(node_id).style
    }

    fn get_block_child_style(&self, child: TNode) -> Self::BlockItemStyle<'_> {
        &self.node(child).style
    }

    fn compute_block_child_layout(
        &mut self,
        node_id: TNode,
        inputs: LayoutInput,
        block_ctx: Option<&mut taffy::BlockContext<'_>>,
    ) -> LayoutOutput {
        self.compute_child(node_id, inputs, block_ctx)
    }
}

impl LayoutFlexboxContainer for BoxTree {
    type FlexboxContainerStyle<'a> = &'a Style;
    type FlexboxItemStyle<'a> = &'a Style;

    fn get_flexbox_container_style(&self, node_id: TNode) -> Self::FlexboxContainerStyle<'_> {
        &self.node(node_id).style
    }

    fn get_flexbox_child_style(&self, child: TNode) -> Self::FlexboxItemStyle<'_> {
        &self.node(child).style
    }
}

impl LayoutGridContainer for BoxTree {
    type GridContainerStyle<'a> = &'a Style;
    type GridItemStyle<'a> = &'a Style;

    fn get_grid_container_style(&self, node_id: TNode) -> Self::GridContainerStyle<'_> {
        &self.node(node_id).style
    }

    fn get_grid_child_style(&self, child: TNode) -> Self::GridItemStyle<'_> {
        &self.node(child).style
    }
}
