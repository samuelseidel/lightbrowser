//! Startup snapshot (ADR 0013): realms deserialized from the snapshot
//! context expose exactly the surface of realms built from templates.
#![cfg(feature = "chrome")]

use std::sync::Arc;

use lb_js::{Engine, EngineOptions, HostRef, JsValue, RealmId, ScriptOrigin, SnapshotStatus};
use lb_persona::{BrowserFamily, Persona};
use lb_webapi::rt::Runtime;
use lb_webapi::{Bindings, Registry, WindowRealmOptions};

/// Every own property of the global, of each interface object and of its
/// prototype: key, kind, attributes, constant values, `length`, `name`
/// and the prototype links. Getters are never invoked.
const SURFACE: &str = r#"(() => {
  const out = [];
  const seen = new Set();
  const label = (o) => o === null ? 'null'
    : (typeof o === 'function' ? 'fn:' + o.name : 'obj:' + (Object.prototype.toString.call(o)));
  function walk(o, path, depth) {
    if (o === null || (typeof o !== 'object' && typeof o !== 'function') || seen.has(o)) return;
    seen.add(o);
    out.push(path + ' __proto__=' + label(Object.getPrototypeOf(o)));
    for (const k of Reflect.ownKeys(o)) {
      const d = Object.getOwnPropertyDescriptor(o, k);
      let line = path + '.' + String(k) + ' ';
      if (d.get || d.set) {
        line += 'a' + (d.get ? 'g' + d.get.name + d.get.length : '') + (d.set ? 's' + d.set.name : '');
      } else {
        const v = d.value;
        line += typeof v;
        if (typeof v === 'function') line += ':' + v.name + '/' + v.length;
        else if (v === null || typeof v !== 'object') line += '=' + String(v);
        if (d.writable) line += 'w';
      }
      if (d.enumerable) line += 'e';
      if (d.configurable) line += 'c';
      out.push(line);
      if (depth < 2 && d.value && (typeof d.value === 'function' || typeof d.value === 'object')) {
        walk(d.value, path + '.' + String(k), depth + 1);
        if (typeof d.value === 'function' && d.value.prototype) {
          walk(d.value.prototype, path + '.' + String(k) + '.prototype', depth + 1);
        }
      }
    }
  }
  walk(globalThis, 'g', 0);
  return out.join('\n');
})()"#;

fn persona() -> Arc<Persona> {
    Arc::new(Persona::default_chrome())
}

/// Made before any other engine of the test process exists (V8 cannot
/// make a snapshot next to live isolates): every test calls this first.
fn blob() -> &'static [u8] {
    lb_webapi::snapshot::startup_snapshot(BrowserFamily::Chrome).expect("snapshot")
}

/// An engine (with or without the snapshot) and its bindings for the full
/// runtime registry.
fn setup(snapshot: bool) -> (Engine, Bindings, Runtime) {
    let blob = blob();
    let mut e = if snapshot {
        Engine::with_snapshot(persona(), EngineOptions::default(), blob)
    } else {
        Engine::new(persona(), EngineOptions::default())
    }
    .unwrap();
    let mut reg = Registry::new();
    let rt = Runtime::new(&mut reg);
    let b = Bindings::define(&mut e, &reg).unwrap();
    rt.set_bindings(b.clone());
    (e, b, rt)
}

fn eval(e: &mut Engine, r: RealmId, src: &str) -> JsValue {
    e.evaluate(r, src, ScriptOrigin::new("https://a.test/t.js"))
        .unwrap_or_else(|err| panic!("{src}: {err}"))
}

fn surface(e: &mut Engine, r: RealmId) -> String {
    match eval(e, r, SURFACE) {
        JsValue::String(s) => s,
        v => panic!("{v:?}"),
    }
}

fn first_diff(a: &str, b: &str) -> Option<(String, String)> {
    a.lines()
        .zip(b.lines())
        .find(|(x, y)| x != y)
        .map(|(x, y)| (x.to_owned(), y.to_owned()))
        .or_else(|| {
            (a.lines().count() != b.lines().count()).then(|| ("<len>".into(), "<len>".into()))
        })
}

#[test]
fn snapshot_realm_matches_built_realm() {
    let (mut built, bb, _rt1) = setup(false);
    let (mut snap, bs, _rt2) = setup(true);
    match snap.snapshot_status() {
        SnapshotStatus::Restored {
            replayed,
            total,
            diverged: false,
            context: true,
        } => assert!(replayed == total && total > bs.len(), "{replayed}/{total}"),
        s => panic!("{s:?}"),
    }
    // The third realm takes the fast global fixup of later snapshot
    // realms with the secure options too.
    for insecure in [false, true, false] {
        let opts = WindowRealmOptions {
            insecure,
            ..Default::default()
        };
        let rb = bb.new_window_realm(&mut built, opts.clone()).unwrap();
        let rs = bs.new_window_realm(&mut snap, opts).unwrap();
        assert!(!lb_js::RealmOps::is_snapshot_realm(&built, rb));
        assert!(lb_js::RealmOps::is_snapshot_realm(&snap, rs));
        let a = surface(&mut built, rb);
        let b = surface(&mut snap, rs);
        assert!(a.lines().count() > 5_000, "{}", a.lines().count());
        if let Ok(dir) = std::env::var("LB_SURFACE_DUMP") {
            std::fs::write(format!("{dir}/built_{insecure}.txt"), &a).unwrap();
            std::fs::write(format!("{dir}/snap_{insecure}.txt"), &b).unwrap();
        }
        assert_eq!(first_diff(&a, &b), None, "insecure={insecure}");
    }
}

#[test]
fn snapshot_realms_work() {
    let (mut e, b, _rt) = setup(true);
    let r1 = b
        .new_window_realm(&mut e, WindowRealmOptions::default())
        .unwrap();
    let r2 = b
        .new_window_realm(&mut e, WindowRealmOptions::default())
        .unwrap();
    // Wrappers use the realm's own interface objects.
    let div = b.interface_id("HTMLDivElement").unwrap();
    for r in [r1, r2] {
        let h = e.wrap(r, div, HostRef::new(1, 7)).unwrap();
        let g = e.global(r).unwrap();
        e.set_property(r, g, "__x", lb_js::Arg::Handle(h)).unwrap();
        assert_eq!(
            eval(
                &mut e,
                r,
                "__x instanceof HTMLDivElement && Object.getPrototypeOf(HTMLDivElement) === HTMLElement \
                 && Object.getPrototypeOf(HTMLDivElement.prototype) === HTMLElement.prototype \
                 && delete globalThis.__x"
            ),
            JsValue::Bool(true)
        );
    }
    // Realms do not share objects.
    eval(&mut e, r1, "Node.prototype.marker = 1; globalThis.y = 2");
    assert_eq!(
        eval(&mut e, r2, "typeof Node.prototype.marker + typeof y"),
        JsValue::String("undefinedundefined".into())
    );
    // Stubs and native functions behave as in a built realm.
    assert_eq!(
        eval(&mut e, r2, "String(Node.prototype.appendChild)"),
        JsValue::String("function appendChild() { [native code] }".into())
    );
    assert_eq!(
        eval(
            &mut e,
            r2,
            "try { new Node() } catch (x) { x instanceof TypeError && x.message }"
        ),
        JsValue::String("Illegal constructor".into())
    );
    assert_eq!(
        eval(
            &mut e,
            r2,
            "Object.getPrototypeOf(DOMException.prototype) === Error.prototype"
        ),
        JsValue::Bool(true)
    );
    // Math.random state is per realm, not baked into the snapshot.
    let a = eval(&mut e, r1, "Math.random()");
    let b2 = eval(&mut e, r2, "Math.random()");
    assert_ne!(a, b2);
}

#[test]
fn other_registry_still_works() {
    // A registry without the runtime: some specs differ (no named getters
    // for Storage etc.); the engine must notice and build those normally.
    let mut e = Engine::with_snapshot(persona(), EngineOptions::default(), blob()).unwrap();
    let b = Bindings::define(&mut e, &Registry::new()).unwrap();
    let r = b
        .new_window_realm(&mut e, WindowRealmOptions::default())
        .unwrap();
    match e.snapshot_status() {
        SnapshotStatus::Restored {
            diverged, context, ..
        } => assert_eq!(context, !diverged),
        s => panic!("{s:?}"),
    }
    assert_eq!(
        eval(&mut e, r, "typeof HTMLDivElement + typeof Storage"),
        JsValue::String("functionfunction".into())
    );
}

#[test]
fn bad_blob_is_rejected() {
    blob();
    let bad: &'static [u8] = b"LBSNAP02 not really a snapshot";
    let e = Engine::with_snapshot(persona(), EngineOptions::default(), bad).unwrap();
    assert!(matches!(e.snapshot_status(), SnapshotStatus::Rejected(_)));
}
