//! Installer tests against the captured Chrome 153 surface (chrome only).
#![cfg(feature = "chrome")]

use std::sync::Arc;
use std::time::Instant;

use lb_js::{Arg, Engine, EngineOptions, HostError, HostRef, JsValue, RealmId, ScriptOrigin};
use lb_persona::Persona;
use lb_webapi::{Bindings, ImplKind, Registry, WindowRealmOptions, host_fn};
use serde_json::Value;

/// The Chrome 153 capture: secure loopback page (default realms) or
/// `about:blank` (non-secure realms).
fn fixture_of(secure: bool) -> Value {
    let name = if secure {
        "chromium_153_surface_secure.json"
    } else {
        "chromium_153_surface.json"
    };
    let path = format!("{}/../../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"));
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn fixture() -> Value {
    fixture_of(true)
}

/// The fixtures were captured on Linux: no Web Share, no Web Bluetooth.
const LINUX_APIS: lb_persona::OsApis = lb_persona::OsApis {
    share: false,
    bluetooth: false,
};

/// An OS-gated window property (`Table::os_gated`).
fn gated_global(b: &Bindings, name: &str) -> bool {
    b.table()
        .os_gated
        .iter()
        .any(|g| g.owner == "Window" && g.name == name)
}

fn setup_with(reg: &Registry, insecure: bool) -> (Engine, Bindings, RealmId) {
    let mut e = engine();
    let b = Bindings::define(&mut e, reg).unwrap();
    let r = b
        .new_window_realm(
            &mut e,
            WindowRealmOptions {
                insecure,
                os_apis: Some(LINUX_APIS),
                ..Default::default()
            },
        )
        .unwrap();
    (e, b, r)
}

fn engine() -> Engine {
    Engine::new(
        Arc::new(Persona::default_chrome()),
        EngineOptions::default(),
    )
    .unwrap()
}

fn setup(reg: &Registry) -> (Engine, Bindings, RealmId) {
    let mut e = engine();
    let b = Bindings::define(&mut e, reg).unwrap();
    let r = b
        .new_window_realm(&mut e, WindowRealmOptions::default())
        .unwrap();
    (e, b, r)
}

fn eval(e: &mut Engine, r: RealmId, src: &str) -> JsValue {
    e.evaluate(r, src, ScriptOrigin::new("https://a.test/t.js"))
        .unwrap_or_else(|err| panic!("{src}: {err}"))
}

fn eval_str(e: &mut Engine, r: RealmId, src: &str) -> String {
    match eval(e, r, src) {
        JsValue::String(s) => s,
        v => panic!("{src}: {v:?}"),
    }
}

fn names(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap()
        .iter()
        .map(|p| p[0].as_str().unwrap().to_owned())
        .collect()
}

#[test]
fn node_prototype_order_matches_fixture() {
    let fx = fixture();
    let (mut e, _, r) = setup(&Registry::new());
    let want = names(&fx["interfaces"]["Node"]["proto"]).join(",");
    let got = eval_str(
        &mut e,
        r,
        "Object.getOwnPropertyNames(Node.prototype).join()",
    );
    assert_eq!(got, want);
}

#[test]
fn prototype_chain() {
    let (mut e, _, r) = setup(&Registry::new());
    for src in [
        "Object.getPrototypeOf(HTMLDivElement.prototype) === HTMLElement.prototype",
        "Object.getPrototypeOf(HTMLElement.prototype) === Element.prototype",
        "Object.getPrototypeOf(Element.prototype) === Node.prototype",
        "Object.getPrototypeOf(Node.prototype) === EventTarget.prototype",
        "Object.getPrototypeOf(EventTarget.prototype) === Object.prototype",
        "Object.getPrototypeOf(HTMLDivElement) === HTMLElement",
        "Object.getPrototypeOf(EventTarget) === Function.prototype",
        "Object.getPrototypeOf(Window.prototype) === EventTarget.prototype",
        "Object.getPrototypeOf(globalThis) === Window.prototype",
        "window.Window === Window && Window.prototype.constructor === Window",
        "Object.getPrototypeOf(DOMException.prototype) === Error.prototype",
        "webkitURL === URL",
        // [LegacyWindowAlias]: the interface object keeps its own name,
        // as in Chrome (the alias comes first on the global).
        "webkitURL.name === 'URL' && URL.name === 'URL'",
        "WebKitMutationObserver.name === 'MutationObserver'",
        "WebKitCSSMatrix.name === 'DOMMatrix' && DOMMatrix.name === 'DOMMatrix'",
        "webkitRTCPeerConnection.name === 'RTCPeerConnection'",
        "webkitMediaStream.name === 'MediaStream'",
        "FeaturePolicy.name === 'PermissionsPolicy'",
        "typeof CSS === 'object' && Object.prototype.toString.call(CSS) === '[object CSS]'",
        "Object.prototype.toString.call(Node.prototype) === '[object Node]'",
        "NodeList.prototype[Symbol.iterator] === Array.prototype.values",
        "NodeList.prototype.forEach === Array.prototype.forEach",
        "Headers.prototype[Symbol.iterator] === Headers.prototype.entries",
        "FontFaceSet.prototype[Symbol.iterator] === FontFaceSet.prototype.values",
        "ReadableStream.prototype[Symbol.asyncIterator] === ReadableStream.prototype.values",
        "Object.getPrototypeOf(Object.getOwnPropertyDescriptor(Element.prototype, Symbol.unscopables).value) === null",
        "Element.prototype[Symbol.unscopables].append === true",
    ] {
        assert_eq!(eval(&mut e, r, src), JsValue::Bool(true), "{src}");
    }
}

#[test]
fn functions_are_native() {
    let (mut e, _, r) = setup(&Registry::new());
    for (src, want) in [
        (
            "Function.prototype.toString.call(Node.prototype.appendChild)",
            "function appendChild() { [native code] }",
        ),
        (
            "Function.prototype.toString.call(Object.getOwnPropertyDescriptor(Node.prototype, 'nodeType').get)",
            "function get nodeType() { [native code] }",
        ),
        (
            "Function.prototype.toString.call(Node)",
            "function Node() { [native code] }",
        ),
        (
            "Function.prototype.toString.call(setTimeout)",
            "function setTimeout() { [native code] }",
        ),
        (
            "Function.prototype.toString.call(CSS.supports)",
            "function supports() { [native code] }",
        ),
        ("String(Node.prototype.appendChild.length)", "1"),
        ("String(Event.length)", "1"),
    ] {
        assert_eq!(eval_str(&mut e, r, src), want, "{src}");
    }
}

#[test]
fn constructor_needs_registration() {
    let (mut e, _, r) = setup(&Registry::new());
    assert_eq!(
        eval_str(
            &mut e,
            r,
            "try { new Event('x'); 'ok' } catch (err) { err.constructor.name + ': ' + err.message }"
        ),
        "TypeError: Illegal constructor"
    );

    let mut reg = Registry::new();
    reg.constructor(
        "Event",
        host_fn(|cx| {
            let t = cx.arg_string(0)?;
            cx.init_this(HostRef::new(7, t.len() as u32))
        }),
    );
    reg.getter(
        "Event",
        "type",
        host_fn(|cx| {
            let h = cx
                .this_host()
                .ok_or_else(|| HostError::type_error("Illegal invocation"))?;
            cx.ret(format!("len{}", h.id))
        }),
    );
    let (mut e, _, r) = setup(&reg);
    assert_eq!(eval_str(&mut e, r, "new Event('abc').type"), "len3");
    assert_eq!(
        eval(&mut e, r, "new Event('abc') instanceof Event"),
        JsValue::Bool(true)
    );
    assert_eq!(
        eval_str(&mut e, r, "try { Event('x') } catch (err) { err.message }"),
        "Failed to construct 'Event': Please use the 'new' operator, this DOM object constructor cannot be called as a function."
    );
}

#[test]
fn unregistered_members_are_stubs() {
    let (mut e, _, r) = setup(&Registry::new());
    let got = eval_str(
        &mut e,
        r,
        "try { document; Node.prototype.appendChild.call(Object.create(Node.prototype)); 'no' } \
         catch (err) { err.constructor.name + ':' + err.message }",
    );
    assert_eq!(got, "TypeError:Illegal invocation");
    let got = eval_str(
        &mut e,
        r,
        "try { alert('x'); 'no' } catch (err) { \
           [err instanceof DOMException, err instanceof Error, err.name, err.code, err.message].join('|') }",
    );
    assert_eq!(
        got,
        "true|true|NotSupportedError|9|Failed to execute 'alert' on 'Window': Not supported."
    );
    // Type-shaped stub getters.
    assert_eq!(eval(&mut e, r, "window.onload"), JsValue::Null);
    assert_eq!(eval(&mut e, r, "window.closed"), JsValue::Bool(false));
    assert_eq!(eval(&mut e, r, "window.innerWidth"), JsValue::Number(0.0));
    assert_eq!(
        eval(&mut e, r, "window.name"),
        JsValue::String(String::new())
    );
    // [Replaceable]: assignment shadows the accessor with a data property.
    assert_eq!(
        eval(
            &mut e,
            r,
            "innerWidth = 5; [innerWidth, typeof Object.getOwnPropertyDescriptor(window, 'innerWidth').value].join()"
        ),
        JsValue::String("5,number".into())
    );
    // DOMException works from JS too.
    assert_eq!(
        eval_str(
            &mut e,
            r,
            "var x = new DOMException('m', 'SyntaxError'); [x.name, x.message, x.code, String(x)].join('|')"
        ),
        "SyntaxError|m|12|SyntaxError: m"
    );
}

#[test]
fn registered_getter_sees_host() {
    let mut reg = Registry::new();
    reg.register(
        "Node",
        "nodeType",
        ImplKind::Getter,
        host_fn(|cx| {
            let h = cx
                .this_host()
                .ok_or_else(|| HostError::type_error("Illegal invocation"))?;
            cx.ret(h.kind * 1000 + h.id)
        }),
    );
    let (mut e, b, r) = setup(&reg);
    let div = b.interface_id("HTMLDivElement").unwrap();
    let f = e
        .evaluate_handle(r, "(function (n) { return [n.nodeType, n instanceof HTMLDivElement, Object.prototype.toString.call(n)].join() })", ScriptOrigin::new("t"))
        .unwrap();
    let v = e
        .call(r, f, Arg::Undefined, &[Arg::Host(div, HostRef::new(1, 42))])
        .unwrap();
    assert_eq!(
        v,
        JsValue::String("1042,true,[object HTMLDivElement]".into())
    );
}

/// Deterministic sample of members, descriptors compared with the fixture
/// kind codes (g/f/v, s, w, e, c).
#[test]
fn descriptors_match_fixture_sample() {
    let fx = fixture();
    let (mut e, b, r) = setup(&Registry::new());
    let mut all = Vec::new();
    for d in b.table().interfaces.iter().filter(|d| d.from_fixture) {
        for p in fx["interfaces"][d.name]["proto"]
            .as_array()
            .into_iter()
            .flatten()
        {
            all.push((
                d.name,
                p[0].as_str().unwrap().to_owned(),
                p[1].as_str().unwrap().to_owned(),
            ));
        }
    }
    let mut seed: u64 = 0x9e37_79b9_7f4a_7c15;
    let mut checked = 0;
    for _ in 0..50 {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        let (iface, name, code) = &all[(seed % all.len() as u64) as usize];
        let got = eval_str(
            &mut e,
            r,
            &format!(
                "(function(){{const d = Object.getOwnPropertyDescriptor({iface}.prototype, {name:?}); \
                 let k = d.get ? 'g' : typeof d.value === 'function' ? 'f' : 'v'; \
                 if (d.set) k += 's'; if (d.writable) k += 'w'; if (d.enumerable) k += 'e'; \
                 if (d.configurable) k += 'c'; return k }})()"
            ),
        );
        assert_eq!(&got, code, "{iface}.{name}");
        checked += 1;
    }
    assert_eq!(checked, 50);
}

/// Every fixture interface: prototype and static own names, symbols,
/// lengths, in secure and non-secure realms. Prints the coverage.
#[test]
fn whole_surface_matches_fixture() {
    surface_matches(true);
    surface_matches(false);
}

fn surface_matches(secure: bool) {
    let fx = fixture_of(secure);
    let (mut e, b, r) = setup_with(&Registry::new(), !secure);
    let mut ok = 0;
    let mut bad = Vec::new();
    let present = |d: &&lb_webapi::desc::InterfaceDesc| {
        d.from_fixture && fx["interfaces"].get(d.name).is_some()
    };
    let total = b.table().interfaces.iter().filter(present).count();
    for d in b.table().interfaces.iter().filter(present) {
        let f = &fx["interfaces"][d.name];
        let want_proto = names(&f["proto"]).join(",");
        let want_static = names(&f["static"]).join(",");
        let want_syms: Vec<String> = f["protoSymbols"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s.as_str().unwrap().to_owned())
            .collect();
        let got = eval_str(
            &mut e,
            r,
            &format!(
                "(function(){{const I = {n}; return [Object.getOwnPropertyNames(I.prototype).join(), \
                 Object.getOwnPropertyNames(I).filter(k => !['length','name','prototype'].includes(k)).join(), \
                 Object.getOwnPropertySymbols(I.prototype).map(String).join(), String(I.length)].join('|') }})()",
                n = d.name
            ),
        );
        let want = format!(
            "{want_proto}|{want_static}|{}|{}",
            want_syms.join(","),
            f["length"]
        );
        if got == want {
            ok += 1;
        } else {
            bad.push(format!("{}\n  got  {got}\n  want {want}", d.name));
        }
    }
    println!("surface (secure={secure}): {ok}/{total} interfaces match the fixture exactly");
    assert!(
        bad.is_empty(),
        "{} mismatches:\n{}",
        bad.len(),
        bad.join("\n")
    );
}

#[test]
fn window_own_props_match_fixture() {
    window_props_match(true);
    window_props_match(false);
}

fn window_props_match(secure: bool) {
    let fx = fixture_of(secure);
    let (mut e, b, r) = setup_with(&Registry::new(), !secure);
    let ours: Vec<&str> = b
        .table()
        .window_props
        .iter()
        .filter(|p| {
            !matches!(
                p,
                lb_webapi::desc::GlobalProp::Builtin(_) | lb_webapi::desc::GlobalProp::Other(_)
            )
        })
        .map(|p| p.name())
        .filter(|n| secure || !b.table().window_secure_only.contains(n))
        .filter(|n| !gated_global(&b, n))
        .collect();
    let got = eval_str(&mut e, r, "Object.getOwnPropertyNames(globalThis).join()");
    let got: Vec<&str> = got.split(',').filter(|n| ours.contains(n)).collect();
    assert_eq!(got, ours);
    // Engine built-ins: report which ones differ from Chrome 153.
    let all = names(&fx["windowProps"]);
    let full = eval_str(&mut e, r, "Object.getOwnPropertyNames(globalThis).join()");
    let full: Vec<&str> = full.split(',').collect();
    let missing: Vec<&String> = all.iter().filter(|n| !full.contains(&n.as_str())).collect();
    let extra: Vec<&&str> = full
        .iter()
        .filter(|n| !all.iter().any(|a| a == *n))
        .collect();
    let same_order = full.iter().zip(&all).take_while(|(a, b)| *a == *b).count();
    if let Ok(p) = std::env::var("LB_DUMP_WINDOW") {
        std::fs::write(p, full.join(",")).unwrap();
    }
    println!(
        "window (secure={secure}): {} own props (Chrome {}), first {} in Chrome's order; missing {:?}; extra {:?}",
        full.len(),
        all.len(),
        same_order,
        missing,
        extra
    );
    // Descriptors of a few window members.
    for (name, code) in [
        ("window", "ge"),
        ("document", "ge"),
        ("location", "gse"),
        ("self", "gsec"),
        ("setTimeout", "fwec"),
        ("Node", "fwc"),
        ("CSS", "vwc"),
    ] {
        let got = eval_str(
            &mut e,
            r,
            &format!(
                "(function(){{const d = Object.getOwnPropertyDescriptor(globalThis, {name:?}); \
                 let k = d.get ? 'g' : typeof d.value === 'function' ? 'f' : 'v'; \
                 if (d.set) k += 's'; if (d.writable) k += 'w'; if (d.enumerable) k += 'e'; \
                 if (d.configurable) k += 'c'; return k }})()"
            ),
        );
        assert_eq!(got, code, "{name}");
    }
}

/// The OS overlay: Windows/macOS members sit where Chrome puts them and
/// realm setup deletes the gates `os_apis` turns off, keeping the order of
/// the rest.
#[test]
fn os_gated_members_follow_os_apis() {
    let reg = Registry::new();
    let mut e = engine();
    let b = Bindings::define(&mut e, &reg).unwrap();
    let realm = |e: &mut Engine, insecure, apis| {
        b.new_window_realm(
            e,
            WindowRealmOptions {
                insecure,
                os_apis: apis,
                ..Default::default()
            },
        )
        .unwrap()
    };
    let nav = "Object.getOwnPropertyNames(Navigator.prototype).join()";
    let win = "Object.getOwnPropertyNames(globalThis).join()";
    let fx = fixture();
    let linux_nav = names(&fx["interfaces"]["Navigator"]["proto"]).join(",");
    let all = lb_persona::OsApis {
        share: true,
        bluetooth: true,
    };

    let r = realm(&mut e, false, Some(all));
    let got = eval_str(&mut e, r, nav);
    let want = linux_nav
        .replace("protectedAudience,", "protectedAudience,bluetooth,")
        .replace(
            "canLoadAdAuctionFencedFrame,",
            "canLoadAdAuctionFencedFrame,canShare,share,",
        );
    assert_eq!(got, want);
    let g = eval_str(&mut e, r, win);
    assert!(
        g.contains("BatteryManager,Bluetooth,BluetoothCharacteristicProperties,BluetoothDevice,")
    );
    assert!(g.contains("BroadcastChannel,BluetoothUUID,BlobEvent"));

    // Linux: exactly the captured surface.
    let r = realm(&mut e, false, Some(LINUX_APIS));
    assert_eq!(eval_str(&mut e, r, nav), linux_nav);
    let g = eval_str(&mut e, r, win);
    assert!(!g.contains("Bluetooth"));

    // Share only (an explicit persona `apis`).
    let r = realm(
        &mut e,
        false,
        Some(lb_persona::OsApis {
            share: true,
            bluetooth: false,
        }),
    );
    let got = eval_str(&mut e, r, nav);
    assert!(got.contains("canShare,share") && !got.contains("bluetooth"));

    // Non-secure Windows realm: only BluetoothUUID (not [SecureContext]).
    let r = realm(&mut e, true, Some(all));
    let g = eval_str(&mut e, r, win);
    assert!(g.contains("BluetoothUUID") && !g.contains("BluetoothDevice"));
    assert_eq!(
        eval_str(
            &mut e,
            r,
            "String('share' in Navigator.prototype) + ('bluetooth' in Navigator.prototype)"
        ),
        "falsefalse"
    );
}

#[test]
fn install_time() {
    let reg = Registry::new();
    // Warm up V8 platform init.
    drop(setup(&reg));
    let t0 = Instant::now();
    let mut e = engine();
    let t1 = Instant::now();
    let b = Bindings::define(&mut e, &reg).unwrap();
    let t2 = Instant::now();
    let _r = b
        .new_window_realm(&mut e, WindowRealmOptions::default())
        .unwrap();
    let t3 = Instant::now();
    let _r2 = b
        .new_window_realm(&mut e, WindowRealmOptions::default())
        .unwrap();
    let t4 = Instant::now();
    let _r3 = e
        .new_realm(lb_js::RealmOptions::with_global(b.window_interface()))
        .unwrap();
    let t5 = Instant::now();
    let _r4 = e
        .new_realm(lb_js::RealmOptions::with_global(b.window_interface()))
        .unwrap();
    let t6 = Instant::now();
    println!(
        "bare realms (no fixup): {:.2} ms, {:.2} ms",
        (t5 - t4).as_secs_f64() * 1e3,
        (t6 - t5).as_secs_f64() * 1e3
    );
    println!(
        "install: engine {:.2} ms, define {} interfaces {:.2} ms, first realm {:.2} ms, second realm {:.2} ms (debug={})",
        (t1 - t0).as_secs_f64() * 1e3,
        b.len(),
        (t2 - t1).as_secs_f64() * 1e3,
        (t3 - t2).as_secs_f64() * 1e3,
        (t4 - t3).as_secs_f64() * 1e3,
        cfg!(debug_assertions)
    );
    if !cfg!(debug_assertions) {
        assert!(
            (t3 - t1).as_millis() < 50,
            "define + realm took {:?}",
            t3 - t1
        );
    }
}
