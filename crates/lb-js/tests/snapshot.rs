//! Startup snapshot (ADR 0013): create, restore, replay, divergence.
#![cfg(feature = "chrome")]

use std::sync::Arc;

use lb_js::{
    Engine, EngineOptions, HostRef, InterfaceId, InterfaceSpec, JsValue, Member, RealmId, RealmOps,
    RealmOptions, ScriptOrigin, SnapshotStatus, host_fn,
};
use lb_persona::Persona;

fn persona() -> Arc<Persona> {
    Arc::new(Persona::default_chrome())
}

const INIT: &str = "globalThis.initRan = (globalThis.initRan || 0) + 1";

/// Node with `id` (getter reading the host id) and `tag()` returning
/// `label`; Element extends Node; Window holds both and a constant.
fn define(e: &mut Engine, label: &'static str, constant: f64) -> InterfaceId {
    let node = e.define_interface(InterfaceSpec::new("Node").prototype(vec![
        Member::getter(
            "id",
            host_fn(|cx| {
                let h = cx
                    .this_host()
                    .ok_or_else(|| lb_js::HostError::type_error("no host"))?;
                cx.ret(h.id)
            }),
        ),
        Member::method("tag", 0, host_fn(move |cx| cx.ret(label))),
        Member::method("realm", 0, host_fn(|cx| cx.ret(cx.realm().0))),
    ]));
    let element = e.define_interface(InterfaceSpec::new("Element").parent(node));
    let window_id = e.next_interface_id();
    e.define_interface(InterfaceSpec::new("Window").instance(vec![
        Member::interface("Window", window_id),
        Member::interface("Node", node),
        Member::interface("Element", element),
        Member::constant("K", JsValue::Number(constant)),
    ]))
}

/// One blob per test process, made before any other engine exists: V8
/// cannot make a snapshot while another isolate is alive.
fn blob() -> &'static [u8] {
    static BLOB: std::sync::OnceLock<&'static [u8]> = std::sync::OnceLock::new();
    BLOB.get_or_init(|| {
        let mut e = Engine::snapshot_creator(persona()).unwrap();
        assert_eq!(e.snapshot_status(), SnapshotStatus::Creating);
        let window = define(&mut e, "creator", 1.0);
        e.create_snapshot(window, INIT, None).unwrap()
    })
}

fn eval(e: &mut Engine, r: RealmId, src: &str) -> JsValue {
    e.evaluate(r, src, ScriptOrigin::new("https://a.test/"))
        .unwrap_or_else(|err| panic!("{src}: {err}"))
}

#[test]
fn restored_realms_use_runtime_callbacks() {
    let blob = blob();
    let mut e = Engine::with_snapshot(persona(), EngineOptions::default(), blob).unwrap();
    // The closures are the restoring engine's, not the creator's.
    let window = define(&mut e, "runtime", 1.0);
    assert!(e.use_snapshot_context(window, INIT, None));
    assert_eq!(
        e.snapshot_status(),
        SnapshotStatus::Restored {
            replayed: 3,
            total: 3,
            diverged: false,
            context: true
        }
    );
    let realms: Vec<RealmId> = (0..3)
        .map(|_| e.new_realm(RealmOptions::with_global(window)).unwrap())
        .collect();
    for (i, &r) in realms.iter().enumerate() {
        assert!(e.is_snapshot_realm(r));
        // The init script ran once, at snapshot time.
        assert_eq!(eval(&mut e, r, "initRan"), JsValue::Number(1.0));
        let el = e
            .wrap(r, InterfaceId(1), HostRef::new(1, 40 + i as u32))
            .unwrap();
        let g = e.global(r).unwrap();
        e.set_property(r, g, "el", lb_js::Arg::Handle(el)).unwrap();
        assert_eq!(
            eval(
                &mut e,
                r,
                "[el.id, el.tag(), el instanceof Node, Object.getPrototypeOf(Element) === Node, K].join()"
            ),
            JsValue::String(format!("{},runtime,true,true,1", 40 + i))
        );
        // Callbacks see the realm they run in.
        assert_eq!(
            eval(&mut e, r, "el.realm()"),
            JsValue::Number(f64::from(r.0))
        );
    }
    // A plain realm is not a snapshot realm.
    let plain = e.new_realm(RealmOptions::default()).unwrap();
    assert!(!e.is_snapshot_realm(plain));
    assert_eq!(
        eval(&mut e, plain, "typeof initRan"),
        JsValue::String("undefined".into())
    );
    // Many dead snapshot contexts at teardown (no GC in between).
    for r in realms {
        e.drop_realm(r);
    }
    for _ in 0..40 {
        let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
        e.drop_realm(r);
    }
    drop(e);
}

#[test]
fn diverging_spec_builds_normally() {
    let blob = blob();
    let mut e = Engine::with_snapshot(persona(), EngineOptions::default(), blob).unwrap();
    // Same shape, different constant: Window diverges.
    let window = define(&mut e, "runtime", 2.0);
    assert!(!e.use_snapshot_context(window, INIT, None));
    assert_eq!(
        e.snapshot_status(),
        SnapshotStatus::Restored {
            replayed: 2,
            total: 3,
            diverged: true,
            context: false
        }
    );
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    assert!(!e.is_snapshot_realm(r));
    assert_eq!(
        eval(&mut e, r, "K + typeof initRan"),
        JsValue::String("2undefined".into())
    );
    let el = e.wrap(r, InterfaceId(1), HostRef::new(1, 5)).unwrap();
    let g = e.global(r).unwrap();
    e.set_property(r, g, "el", lb_js::Arg::Handle(el)).unwrap();
    assert_eq!(
        eval(&mut e, r, "el.tag() + el.id"),
        JsValue::String("runtime5".into())
    );
}

#[test]
fn other_init_script_skips_snapshot_context() {
    let blob = blob();
    let mut e = Engine::with_snapshot(persona(), EngineOptions::default(), blob).unwrap();
    let window = define(&mut e, "runtime", 1.0);
    assert!(!e.use_snapshot_context(window, "other()", None));
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    assert!(!e.is_snapshot_realm(r));
}

#[test]
fn creator_only_api() {
    blob();
    let e = Engine::new(persona(), EngineOptions::default()).unwrap();
    assert_eq!(e.snapshot_status(), SnapshotStatus::None);
    assert!(e.create_snapshot(InterfaceId(0), "", None).is_err());
}

#[test]
fn one_creator_per_process() {
    blob();
    assert!(matches!(
        Engine::snapshot_creator(persona()),
        Err(lb_js::EngineError::SnapshotCreatorUsed)
    ));
}
