//! Form-associated custom elements and `ElementInternals` (ADR 0026).

use std::rc::Rc;

use lb_dom::{Mutation, NodeId};

use super::{Js, Runtime, Window};
use crate::registry::Registry;

/// A form-associated element was upgraded: reset its form owner.
pub(crate) fn upgraded(_js: &mut dyn Js, _w: &Rc<Window>, _slot: u32, _n: NodeId) {}

/// Form owner changes after DOM changes.
pub(crate) fn on_mutations(_w: &Rc<Window>, _batch: &[(u32, Mutation)]) {}

pub(crate) fn register(_reg: &mut Registry, _rt: &Runtime) {}
