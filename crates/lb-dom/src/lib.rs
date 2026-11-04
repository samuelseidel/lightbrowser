//! Arena DOM for LightBrowser.
//!
//! - [`Document`]: one arena of compact nodes addressed by [`NodeId`];
//!   dropping the document frees every node in one step.
//! - Tree, attribute and text APIs with DOM semantics and error types
//!   ([`DomError`]) the binding layer maps to `DOMException`s.
//! - [`HtmlParser`]: streaming html5ever driver that yields at each
//!   `</script>` ([`ParseStep::Script`]) and supports `document.write`.
//! - Fragment parsing (`innerHTML`), serialization (`outerHTML`).
//! - Selector matching with Servo `selectors`.
//! - [`MutationHook`] for MutationObserver and script insertion.
//!
//! No JS here; the bindings layer calls these APIs.

mod document;
mod mutation;
mod node;
mod parser;
mod select;
mod serialize;
mod sink;

pub use document::{Children, Descendants, Document, DomError};
pub use mutation::{Mutation, MutationHook};
pub use node::{Doctype, ElementData, NodeFlags, NodeId, NodeType, ProcessingInstruction};
pub use parser::{HtmlParser, ParseStep, ParserOptions};
pub use select::{ElementFacts, ElementRef, SelectorList};
pub use serialize::SerializeNode;

/// Interned names and helpers from html5ever, re-exported so callers use
/// the same atom types.
pub use html5ever::tendril::StrTendril;
pub use html5ever::tree_builder::QuirksMode;
pub use html5ever::{
    Attribute, LocalName, Namespace, Prefix, QualName, local_name, namespace_url, ns,
};

#[doc(hidden)]
pub fn node_struct_size() -> usize {
    std::mem::size_of::<node::Node>()
}
