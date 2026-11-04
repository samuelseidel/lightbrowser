//! Custom elements (ADR 0026): `customElements`, definitions, reactions,
//! upgrades, customized built-ins and form-associated elements.

use lb_dom::NodeId;

use super::Window;

/// The `is` value of element `n` (customized built-ins), if any.
pub(crate) fn is_value(_w: &Window, _slot: u32, _n: NodeId) -> Option<String> {
    None
}

/// True if `n`'s definition disables shadow roots (`disabledFeatures`).
pub(crate) fn shadow_disabled(_w: &Window, _slot: u32, _n: NodeId) -> bool {
    false
}
