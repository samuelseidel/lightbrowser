//! Font faces that `local()` resolves on a default install of an OS
//! release (`profiles/local_fonts/<os>_<version>.json`, see its README):
//! full and PostScript names, grouped by the family CSS matches them by.
//!
//! Chrome resolves `FontFace` / `@font-face` `local("name")` through the
//! platform's unique-name lookup: CoreText on macOS, DirectWrite's
//! PostScript then full-name property match on Windows, fontconfig's
//! `FC_POSTSCRIPT_NAME` then `FC_FULLNAME` on Linux. All compare without
//! case. A family name that is not also a face name does not resolve
//! (`local("Menlo")` fails on macOS, `local("Menlo Regular")` loads).
//!
//! The lists are bundled data, not persona values: the persona's OS
//! release picks one ([`LocalFonts::for_persona`]). They are packed into
//! one zstd frame by `build.rs` (about 15 KB); a list is unpacked and
//! parsed on its first use only.

use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

use lb_persona::{Distro, OsKind, PersonaProfile};
use serde::Deserialize;

include!(concat!(env!("OUT_DIR"), "/local_fonts.rs"));

#[derive(Deserialize)]
struct ListFile {
    format: u32,
    families: BTreeMap<String, Vec<String>>,
}

/// One OS release's `local()` faces.
#[derive(Debug)]
pub struct LocalFonts {
    id: &'static str,
    families: Vec<String>,
    /// Lowercase face name -> index into `families`.
    faces: HashMap<String, usize>,
    /// Lowercase family name -> index into `families`.
    family_index: HashMap<String, usize>,
}

impl LocalFonts {
    /// The list for the persona's OS release, `None` when there is none
    /// (an OS or distro without a list).
    pub fn for_persona(p: &PersonaProfile) -> Option<&'static LocalFonts> {
        Self::get(list_id(p)?)
    }

    /// A bundled list by id (`macos_26`, `windows_11`, `ubuntu_24.04`).
    pub fn get(id: &str) -> Option<&'static LocalFonts> {
        static PARSED: OnceLock<Vec<OnceLock<LocalFonts>>> = OnceLock::new();
        let i = LOCAL_LISTS.iter().position(|(n, _)| *n == id)?;
        let slots = PARSED.get_or_init(|| LOCAL_LISTS.iter().map(|_| OnceLock::new()).collect());
        Some(slots[i].get_or_init(|| {
            let (id, range) = &LOCAL_LISTS[i];
            LocalFonts::parse(id, &unpacked()[range.clone()])
        }))
    }

    /// Ids of every bundled list.
    pub fn ids() -> impl Iterator<Item = &'static str> {
        LOCAL_LISTS.iter().map(|(id, _)| *id)
    }

    fn parse(id: &'static str, json: &str) -> LocalFonts {
        let f: ListFile = serde_json::from_str(json).expect("bundled local font list");
        debug_assert_eq!(f.format, 2, "{id}");
        let mut out = LocalFonts {
            id,
            families: Vec::with_capacity(f.families.len()),
            faces: HashMap::new(),
            family_index: HashMap::new(),
        };
        for (family, names) in f.families {
            let i = out.families.len();
            out.family_index.insert(family.to_lowercase(), i);
            for n in names {
                out.faces.entry(n.to_lowercase()).or_insert(i);
            }
            out.families.push(family);
        }
        out
    }

    pub fn id(&self) -> &'static str {
        self.id
    }

    /// The family of the face `name` (full or PostScript name, any case),
    /// `None` when `local(name)` does not resolve.
    pub fn face_family(&self, name: &str) -> Option<&str> {
        self.faces
            .get(&name.to_lowercase())
            .map(|&i| self.families[i].as_str())
    }

    /// Whether the default install has the family (any case).
    pub fn has_family(&self, family: &str) -> bool {
        self.family_index.contains_key(&family.to_lowercase())
    }

    /// Family names, sorted.
    pub fn families(&self) -> impl Iterator<Item = &str> {
        self.families.iter().map(String::as_str)
    }

    /// Number of face names.
    pub fn len(&self) -> usize {
        self.faces.len()
    }

    pub fn is_empty(&self) -> bool {
        self.faces.is_empty()
    }
}

/// The unpacked JSON of every list. Decompressed per parsed list (about
/// 0.5 ms) and dropped after it, so only the parsed list stays in memory.
fn unpacked() -> String {
    let raw = zstd::bulk::decompress(PACKED_LOCAL, PACKED_LOCAL_LEN)
        .expect("bundled local font lists decompress");
    String::from_utf8(raw).expect("bundled local font lists are UTF-8")
}

/// The list id for the persona's OS release: `os.name` and `os.version`
/// (Windows: the build number when it has one, 22000 and later is 11;
/// Linux: `os.distro`, else the distro named in `os.version`). Releases
/// between the bundled ones take the closest older list; macOS before 13
/// takes 13's.
pub fn list_id(p: &PersonaProfile) -> Option<&'static str> {
    match p.os_kind() {
        OsKind::Windows => {
            let eleven = match lb_persona::os::windows_build_number(&p.os.build) {
                Some(b) if b > 0 => b >= 22000,
                _ => p.windows_major()? >= 11,
            };
            Some(if eleven { "windows_11" } else { "windows_10" })
        }
        OsKind::MacOs => {
            let major = p.macos_version().map_or(0, |v| v.0);
            Some(match major {
                26.. => "macos_26",
                15 => "macos_15",
                14 => "macos_14",
                _ => "macos_13",
            })
        }
        OsKind::Linux => {
            let (distro, version) = p
                .linux_distro()
                .or_else(|| distro_from_version(&p.os.version))?;
            let major: u32 = version
                .split('.')
                .next()
                .and_then(|m| m.parse().ok())
                .unwrap_or(0);
            match distro {
                Distro::Ubuntu if major >= 24 => Some("ubuntu_24.04"),
                Distro::Ubuntu => Some("ubuntu_22.04"),
                Distro::Fedora if major >= 42 => Some("fedora_42"),
                Distro::Fedora => Some("fedora_41"),
                Distro::Other => None,
            }
        }
        _ => None,
    }
}

/// `Ubuntu 24.04 LTS` -> Ubuntu 24.04, `Fedora Linux 42 (Workstation Edition)`
/// -> Fedora 42.
fn distro_from_version(v: &str) -> Option<(Distro, String)> {
    let lower = v.to_ascii_lowercase();
    let kind = if lower.starts_with("ubuntu") {
        Distro::Ubuntu
    } else if lower.starts_with("fedora") {
        Distro::Fedora
    } else {
        return None;
    };
    let ver = lower
        .split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))?;
    Some((kind, ver.to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_list_parses() {
        for id in LocalFonts::ids() {
            let l = LocalFonts::get(id).unwrap();
            assert!(!l.is_empty(), "{id}");
            assert_eq!(l.id(), id);
        }
        assert!(LocalFonts::get("nope").is_none());
    }

    #[test]
    fn names_match_without_case() {
        let mac = LocalFonts::get("macos_26").unwrap();
        assert_eq!(mac.face_family("ARIALMT"), Some("Arial"));
        assert_eq!(mac.face_family("menlo regular"), Some("Menlo"));
        assert_eq!(mac.face_family("Menlo"), None);
        assert!(mac.has_family("menlo"));
    }
}
