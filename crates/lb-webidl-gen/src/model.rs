//! Merged IDL model: partial interfaces and `includes` mixins folded into
//! their interface, with the extended attributes of the container
//! (partial / mixin) kept per member.

use std::collections::{BTreeMap, BTreeSet};

use crate::parse::{DefKind, Definition, ExtAttrs, IdlFile, Member};

/// Where a member was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Own,
    Partial,
    Mixin(String),
}

/// A member with its declaration context.
#[derive(Debug, Clone)]
pub struct MemberInfo {
    pub member: Member,
    pub source: Source,
    /// Extended attributes of the partial or mixin that declared it
    /// (empty for own members). For a mixin member declared in a partial
    /// mixin, the partial's attributes come first.
    pub container_ext: Vec<ExtAttrs>,
    pub file: String,
}

impl MemberInfo {
    /// `[RuntimeEnabled]` features gating this member (member and
    /// container; all must be on).
    pub fn features(&self) -> Vec<String> {
        let mut out: Vec<String> = self.member.ext.idents("RuntimeEnabled");
        for e in &self.container_ext {
            out.extend(e.idents("RuntimeEnabled"));
        }
        out
    }

    /// The member's own extended attribute, else its container's.
    pub fn ext_has(&self, name: &str) -> bool {
        self.member.ext.has(name) || self.container_ext.iter().any(|e| e.has(name))
    }

    /// Effective `[Exposed]` globals: member, then container; `None` means
    /// "same as the interface".
    pub fn exposed(&self) -> Option<Vec<String>> {
        let own = self.member.ext.idents("Exposed");
        if !own.is_empty() {
            return Some(own);
        }
        for e in &self.container_ext {
            let v = e.idents("Exposed");
            if !v.is_empty() {
                return Some(v);
            }
        }
        None
    }
}

#[derive(Debug, Clone)]
pub struct Interface {
    pub name: String,
    pub kind: DefKind,
    pub parent: Option<String>,
    pub ext: ExtAttrs,
    pub file: String,
    pub members: Vec<MemberInfo>,
}

impl Interface {
    pub fn features(&self) -> Vec<String> {
        self.ext.idents("RuntimeEnabled")
    }
}

/// All definitions of one IDL set.
#[derive(Debug, Default)]
pub struct Model {
    /// Interfaces, callback interfaces and namespaces.
    pub interfaces: BTreeMap<String, Interface>,
    pub dictionaries: BTreeSet<String>,
    pub enums: BTreeSet<String>,
    pub typedefs: BTreeSet<String>,
    pub callbacks: BTreeSet<String>,
    /// Problems found while merging (missing mixins, orphan partials).
    pub warnings: Vec<String>,
}

impl Model {
    pub fn build(files: &[IdlFile]) -> Model {
        let mut m = Model::default();
        let mut mixins: BTreeMap<String, (Definition, Vec<MemberInfo>)> = BTreeMap::new();
        let mut partials: Vec<&Definition> = Vec::new();
        let mut partial_mixins: Vec<&Definition> = Vec::new();

        for d in files.iter().flat_map(|f| &f.definitions) {
            match d.kind {
                DefKind::Interface | DefKind::CallbackInterface | DefKind::Namespace => {
                    if d.partial {
                        partials.push(d);
                    } else {
                        let members = d
                            .members
                            .iter()
                            .map(|mem| MemberInfo {
                                member: mem.clone(),
                                source: Source::Own,
                                container_ext: Vec::new(),
                                file: d.file.clone(),
                            })
                            .collect();
                        if let Some(old) = m.interfaces.insert(
                            d.name.clone(),
                            Interface {
                                name: d.name.clone(),
                                kind: d.kind,
                                parent: d.parent.clone(),
                                ext: d.ext.clone(),
                                file: d.file.clone(),
                                members,
                            },
                        ) {
                            m.warnings
                                .push(format!("{} defined twice ({})", d.name, old.file));
                        }
                    }
                }
                DefKind::Mixin => {
                    if d.partial {
                        partial_mixins.push(d);
                    } else {
                        let members = d
                            .members
                            .iter()
                            .map(|mem| MemberInfo {
                                member: mem.clone(),
                                source: Source::Mixin(d.name.clone()),
                                container_ext: vec![d.ext.clone()],
                                file: d.file.clone(),
                            })
                            .collect();
                        mixins.insert(d.name.clone(), (d.clone(), members));
                    }
                }
                DefKind::Dictionary => {
                    m.dictionaries.insert(d.name.clone());
                }
                DefKind::Enum => {
                    m.enums.insert(d.name.clone());
                }
                DefKind::Typedef => {
                    m.typedefs.insert(d.name.clone());
                }
                DefKind::Callback => {
                    m.callbacks.insert(d.name.clone());
                }
            }
        }

        for d in partial_mixins {
            let Some((base, members)) = mixins.get_mut(&d.name) else {
                m.warnings
                    .push(format!("partial mixin {} without mixin", d.name));
                continue;
            };
            for mem in &d.members {
                members.push(MemberInfo {
                    member: mem.clone(),
                    source: Source::Mixin(d.name.clone()),
                    container_ext: vec![d.ext.clone(), base.ext.clone()],
                    file: d.file.clone(),
                });
            }
        }

        for d in partials {
            let Some(iface) = m.interfaces.get_mut(&d.name) else {
                m.warnings
                    .push(format!("partial {} without interface", d.name));
                continue;
            };
            for mem in &d.members {
                iface.members.push(MemberInfo {
                    member: mem.clone(),
                    source: Source::Partial,
                    container_ext: vec![d.ext.clone()],
                    file: d.file.clone(),
                });
            }
        }

        for inc in files.iter().flat_map(|f| &f.includes) {
            let Some((_, members)) = mixins.get(&inc.mixin) else {
                m.warnings.push(format!("unknown mixin {}", inc.mixin));
                continue;
            };
            let Some(iface) = m.interfaces.get_mut(&inc.target) else {
                m.warnings.push(format!(
                    "{} includes {} but is undefined",
                    inc.target, inc.mixin
                ));
                continue;
            };
            iface.members.extend(members.iter().cloned());
        }
        m
    }

    /// The parent chain, nearest first.
    pub fn ancestors(&self, name: &str) -> Vec<&Interface> {
        let mut out = Vec::new();
        let mut cur = self.interfaces.get(name).and_then(|i| i.parent.as_deref());
        while let Some(p) = cur {
            let Some(i) = self.interfaces.get(p) else {
                break;
            };
            out.push(i);
            cur = i.parent.as_deref();
        }
        out
    }
}
