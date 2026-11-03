//! Generator tests: whole-set parse, Chrome order for key interfaces,
//! coverage report, and the checked-in table being up to date.

use std::collections::HashSet;
use std::sync::OnceLock;

use lb_webidl_gen::ir::{GlobalProp, Slot, Table};
use lb_webidl_gen::{ChromeInputs, build_chrome, parse_dir, render_chrome};
use serde_json::Value;

fn table() -> &'static Table {
    static T: OnceLock<Table> = OnceLock::new();
    T.get_or_init(|| {
        let i = ChromeInputs::default();
        build_chrome(&i).expect("build")
    })
}

fn fixture() -> &'static Value {
    static F: OnceLock<Value> = OnceLock::new();
    F.get_or_init(|| {
        let i = ChromeInputs::default();
        serde_json::from_str(&std::fs::read_to_string(i.fixture).unwrap()).unwrap()
    })
}

fn fixture_names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| p[0].as_str().unwrap().to_owned())
        .filter(|n| n != "constructor")
        .collect()
}

/// Prototype names in emitted order, with late members after
/// `constructor` (dropped here) as the installer arranges them.
fn proto_names(name: &str) -> Vec<String> {
    let i = table()
        .interface(name)
        .unwrap_or_else(|| panic!("{name} missing"));
    let late = |s: &Slot| match *s {
        Slot::Attr(x) => i.attributes[x].late,
        Slot::Op(x) => i.operations[x].late,
        Slot::Const(_) => false,
    };
    let early = i.proto_order.iter().filter(|s| !late(s));
    let later = i.proto_order.iter().filter(|s| late(s));
    early
        .chain(later)
        .map(|s| i.slot_name(*s).to_owned())
        .collect()
}

#[test]
fn parses_whole_vendored_set() {
    let i = ChromeInputs::default();
    let files = parse_dir(&i.idl_root).expect("every vendored file parses");
    assert_eq!(files.len(), 2229);
    let defs: usize = files.iter().map(|f| f.definitions.len()).sum();
    assert!(defs > 1800, "{defs} definitions");
}

#[test]
fn key_interfaces_in_chrome_order() {
    for name in [
        "Node",
        "Element",
        "Document",
        "HTMLElement",
        "Navigator",
        "EventTarget",
        "Event",
    ] {
        let want = fixture_names(&fixture()["interfaces"][name]["proto"]);
        assert_eq!(proto_names(name), want, "{name}.prototype");
        let i = table().interface(name).unwrap();
        let statics: Vec<&str> = i.static_order.iter().map(|s| i.slot_name(*s)).collect();
        assert_eq!(
            statics,
            fixture_names(&fixture()["interfaces"][name]["static"]),
            "{name} statics"
        );
    }
    let node = table().interface("Node").unwrap();
    assert_eq!(node.parent.as_deref(), Some("EventTarget"));
    assert_eq!(table().interface("Event").unwrap().constructor_length, 1);
}

#[test]
fn window_own_props_in_chrome_order() {
    let t = table();
    let want: Vec<String> = fixture()["windowProps"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p[0].as_str().unwrap().to_owned())
        .collect();
    let got: Vec<&str> = t.window_props.iter().map(GlobalProp::name).collect();
    assert_eq!(got, want);
    // Attributes and operations of the global, in the same order.
    let w = t.interface("Window").unwrap();
    let members: Vec<&str> = w.instance_order.iter().map(|s| w.slot_name(*s)).collect();
    let props: Vec<&str> = t
        .window_props
        .iter()
        .filter(|p| matches!(p, GlobalProp::Attr(_) | GlobalProp::Op(_)))
        .map(GlobalProp::name)
        .collect();
    assert_eq!(members, props);
    assert!(members.starts_with(&["window", "self", "document", "name", "location"]));
    let location = &w.attributes[w
        .attributes
        .iter()
        .position(|a| a.name == "location")
        .unwrap()];
    assert!(location.unforgeable && location.put_forwards.as_deref() == Some("href"));
    assert!(matches!(
        t.window_props.iter().find(|p| p.name() == "webkitURL"),
        Some(GlobalProp::Alias { target, .. }) if target == "URL"
    ));
    assert!(matches!(
        t.window_props.iter().find(|p| p.name() == "Image"),
        Some(GlobalProp::LegacyFactory { iface, .. }) if iface == "HTMLImageElement"
    ));
    assert_eq!(t.window_props[t.window_late_start].name(), "Temporal");
}

#[test]
fn every_fixture_interface_reproduced() {
    let t = table();
    let fx = fixture();
    let mut checked = 0;
    for i in t.interfaces.iter().filter(|i| i.from_fixture) {
        let f = &fx["interfaces"][i.name.as_str()];
        assert_eq!(
            proto_names(&i.name),
            fixture_names(&f["proto"]),
            "{}",
            i.name
        );
        assert_eq!(
            i.constructor_length as u64,
            f["length"].as_u64().unwrap(),
            "{}",
            i.name
        );
        checked += 1;
    }
    assert!(checked >= 670, "{checked}");
    // Parents come before children.
    let mut seen = HashSet::new();
    for i in &t.interfaces {
        if let Some(p) = &i.parent {
            assert!(seen.contains(p), "{} before its parent {p}", i.name);
        }
        seen.insert(i.name.clone());
    }
}

#[test]
fn coverage_report() {
    let c = &table().coverage;
    println!("coverage: {}", c.summary());
    for w in &c.warnings {
        println!("  warning: {w}");
    }
    assert_eq!(c.fixture_interfaces, c.idl_interfaces);
    assert!(
        c.idl_members * 100 >= c.fixture_members * 99,
        "{}",
        c.summary()
    );
    assert!(
        c.exact_order * 10 >= c.fixture_interfaces * 8,
        "{}",
        c.summary()
    );
}

#[test]
fn generated_table_is_up_to_date() {
    let i = ChromeInputs::default();
    let current = std::fs::read_to_string(&i.out).unwrap_or_default();
    assert!(
        current == render_chrome(table()),
        "{} is stale: run `cargo run -p lb-webidl-gen -- --family chrome`",
        i.out.display()
    );
}
