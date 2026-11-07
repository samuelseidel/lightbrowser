//! Mutation hooks: the page layer observes tree changes here to drive
//! `MutationObserver` and to run scripts that get inserted by script.

use html5ever::QualName;
use html5ever::tendril::StrTendril;

use crate::NodeId;

/// One tree change. Fired synchronously, after the change is applied.
///
/// Changes made by the HTML parser fire too (`NodeFlags::PARSER_INSERTED`
/// marks parser-created elements). Text the parser merges into an existing
/// Text node does not fire `CharacterData`.
#[derive(Clone, Debug)]
pub enum Mutation {
    ChildInserted {
        parent: NodeId,
        node: NodeId,
        prev_sibling: Option<NodeId>,
        next_sibling: Option<NodeId>,
    },
    ChildRemoved {
        parent: NodeId,
        node: NodeId,
        prev_sibling: Option<NodeId>,
        next_sibling: Option<NodeId>,
    },
    /// An attribute was added (`old_value: None`), changed or removed.
    Attribute {
        element: NodeId,
        name: QualName,
        old_value: Option<StrTendril>,
    },
    CharacterData {
        node: NodeId,
        old_value: StrTendril,
    },
}

/// Receives [`Mutation`]s. It gets no access to the document, so it must
/// queue work (MutationObserver records, script preparation) for later.
pub trait MutationHook {
    fn on_mutation(&mut self, mutation: &Mutation);
}

impl<F: FnMut(&Mutation)> MutationHook for F {
    fn on_mutation(&mut self, mutation: &Mutation) {
        self(mutation)
    }
}
