//! V8 backend tests (chrome feature). Everything runs in-process; no network.
#![cfg(feature = "chrome")]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;

use lb_js::{
    Arg, Engine, EngineError, EngineOptions, HostError, HostRef, InterfaceId, InterfaceSpec,
    JsEngine, JsErrorKind, JsValue, Member, ModuleSource, Realm, RealmId, RealmOptions,
    ScriptOrigin, World, host_fn,
};
use lb_persona::Persona;

fn engine() -> Engine {
    Engine::new(
        Arc::new(Persona::default_chrome()),
        EngineOptions::default(),
    )
    .unwrap()
}

fn origin() -> ScriptOrigin<'static> {
    ScriptOrigin::new("https://a.test/x.js")
}

fn eval(e: &mut Engine, realm: RealmId, src: &str) -> JsValue {
    e.evaluate(realm, src, origin())
        .unwrap_or_else(|err| panic!("{src}: {err}"))
}

fn plain_realm(e: &mut Engine) -> RealmId {
    e.new_realm(RealmOptions::default()).unwrap()
}

const NODE_KIND: u32 = 1;

/// Node (with a `nodeId` getter and `describe()` method), Element extends
/// Node, Window as the global with `Node`, `Element` and `makeNode(id)`.
struct Dom {
    node: InterfaceId,
    element: InterfaceId,
    window: InterfaceId,
}

fn define_dom(e: &mut Engine) -> Dom {
    let node = e.define_interface(
        InterfaceSpec::new("Node")
            .statics(vec![Member::constant("ELEMENT_NODE", JsValue::Number(1.0))])
            .prototype(vec![
                Member::constant("ELEMENT_NODE", JsValue::Number(1.0)),
                Member::getter(
                    "nodeId",
                    host_fn(|cx| {
                        let h = cx
                            .this_host()
                            .ok_or_else(|| HostError::type_error("Illegal invocation"))?;
                        cx.ret(h.id)
                    }),
                ),
                Member::method(
                    "describe",
                    1,
                    host_fn(|cx| {
                        let h = cx
                            .this_host()
                            .ok_or_else(|| HostError::type_error("Illegal invocation"))?;
                        let suffix = cx.arg_string(0)?;
                        cx.ret(format!("{}:{}:{suffix}", h.kind, h.id))
                    }),
                ),
            ]),
    );
    let element = e.define_interface(InterfaceSpec::new("Element").parent(node));
    let window = e.define_interface(InterfaceSpec::new("Window").instance(vec![
        Member::interface("Node", node),
        Member::interface("Element", element),
        Member::method(
            "makeNode",
            1,
            host_fn(move |cx| {
                let id = cx.arg_u32(0)?;
                cx.ret(Arg::Host(element, HostRef::new(NODE_KIND, id)))
            }),
        ),
        Member::method(
            "nodeIdOf",
            1,
            host_fn(|cx| {
                let h = cx.arg_host(0);
                cx.ret(h.map_or(-1.0, |h| f64::from(h.id)))
            }),
        ),
    ]));
    Dom {
        node,
        element,
        window,
    }
}

#[test]
fn evaluates_arithmetic() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    assert_eq!(eval(&mut e, r, "1 + 2 * 3"), JsValue::Number(7.0));
    assert_eq!(eval(&mut e, r, "'a' + 1"), JsValue::String("a1".into()));
    assert_eq!(
        eval(&mut e, r, "({a: [1, null]})"),
        JsValue::Json(r#"{"a":[1,null]}"#.into())
    );
    assert_eq!(
        eval(&mut e, r, "Symbol('s')"),
        JsValue::Opaque("Symbol(s)".into())
    );
    assert_eq!(eval(&mut e, r, "undefined"), JsValue::Undefined);
}

#[test]
fn trait_realm_evaluates() {
    let mut e = engine();
    let id = JsEngine::new_realm(&mut e, RealmOptions::default()).unwrap();
    let mut realm = e.realm(id).unwrap();
    assert_eq!(
        realm.evaluate("6 * 7", origin()).unwrap(),
        JsValue::Number(42.0)
    );
    assert_eq!(realm.id(), id);
    e.drop_realm(id);
    assert!(e.realm(id).is_none());
}

#[test]
fn rejects_firefox_persona() {
    let err = Engine::new(
        Arc::new(Persona::default_firefox()),
        EngineOptions::default(),
    )
    .err()
    .unwrap();
    assert!(matches!(err, EngineError::FamilyMismatch { .. }));
}

#[test]
fn null_property_error_matches_v8() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    let err = e.evaluate(r, "null.x", origin()).unwrap_err();
    assert_eq!(err.kind, JsErrorKind::Exception);
    assert_eq!(err.message, "Cannot read properties of null (reading 'x')");
    let stack = err.stack.unwrap();
    let mut lines = stack.lines();
    assert_eq!(
        lines.next(),
        Some("TypeError: Cannot read properties of null (reading 'x')")
    );
    assert_eq!(lines.next(), Some("    at https://a.test/x.js:1:6"));
    assert_eq!(err.url.as_deref(), Some("https://a.test/x.js"));
    assert_eq!(err.line, Some(1));
}

#[test]
fn line_offset_shows_in_stack() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    let src = "function f() {\n  null.x;\n}\nf();";
    let err = e
        .evaluate(
            r,
            src,
            ScriptOrigin::new("https://a.test/page.html").at(9, 0),
        )
        .unwrap_err();
    let stack = err.stack.unwrap();
    let frames: Vec<&str> = stack.lines().skip(1).collect();
    assert_eq!(frames[0], "    at f (https://a.test/page.html:11:8)");
    assert_eq!(frames[1], "    at https://a.test/page.html:13:1");
    assert_eq!(err.line, Some(11));
}

#[test]
fn syntax_error_and_thrown_values() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    let err = e.evaluate(r, "let = ;", origin()).unwrap_err();
    assert_eq!(err.kind, JsErrorKind::Exception);
    assert!(err.stack.unwrap().starts_with("SyntaxError: "));
    let err = e.evaluate(r, "throw 42", origin()).unwrap_err();
    assert_eq!(err.message, "42");
    assert_eq!(err.stack, None);
}

#[test]
fn rust_global_function_is_callable() {
    let mut e = engine();
    let calls = Rc::new(Cell::new(0));
    let c = calls.clone();
    let window = e.define_interface(InterfaceSpec::new("Window").instance(vec![Member::method(
        "add",
        2,
        host_fn(move |cx| {
            c.set(c.get() + 1);
            let a = cx.arg_f64(0)?;
            let b = cx.arg_f64(1)?;
            cx.ret(a + b)
        }),
    )]));
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    assert_eq!(eval(&mut e, r, "add(2, 40)"), JsValue::Number(42.0));
    assert_eq!(eval(&mut e, r, "add.length"), JsValue::Number(2.0));
    assert_eq!(eval(&mut e, r, "add.name"), JsValue::String("add".into()));
    assert_eq!(
        eval(&mut e, r, "Function.prototype.toString.call(add)"),
        JsValue::String("function add() { [native code] }".into())
    );
    assert_eq!(eval(&mut e, r, "'prototype' in add"), JsValue::Bool(false));
    assert_eq!(calls.get(), 1);
    let err = e.evaluate(r, "new add()", origin()).unwrap_err();
    assert_eq!(err.message, "add is not a constructor");
    // A throwing conversion propagates the exception unchanged.
    let err = e
        .evaluate(
            r,
            "add({valueOf() { throw new RangeError('boom') }}, 1)",
            origin(),
        )
        .unwrap_err();
    assert_eq!(err.message, "boom");
    assert!(err.stack.unwrap().starts_with("RangeError: boom"));
}

#[test]
fn host_errors_become_js_exceptions() {
    let mut e = engine();
    let window = e.define_interface(InterfaceSpec::new("Window").instance(vec![Member::method(
        "fail",
        0,
        host_fn(|_| Err(HostError::type_error("nope"))),
    )]));
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    assert_eq!(
        eval(
            &mut e,
            r,
            "try { fail() } catch (e) { e instanceof TypeError && e.message }"
        ),
        JsValue::String("nope".into())
    );
}

#[test]
fn wrapped_host_object_keeps_identity() {
    let mut e = engine();
    let dom = define_dom(&mut e);
    let r = e.new_realm(RealmOptions::with_global(dom.window)).unwrap();
    assert_eq!(
        eval(&mut e, r, "makeNode(7) === makeNode(7)"),
        JsValue::Bool(true)
    );
    assert_eq!(
        eval(&mut e, r, "makeNode(7) === makeNode(8)"),
        JsValue::Bool(false)
    );
    assert_eq!(
        eval(&mut e, r, "makeNode(7).expando = 1; makeNode(7).expando"),
        JsValue::Number(1.0)
    );
    assert_eq!(
        eval(&mut e, r, "nodeIdOf(makeNode(9))"),
        JsValue::Number(9.0)
    );
    assert_eq!(eval(&mut e, r, "nodeIdOf({})"), JsValue::Number(-1.0));

    // From Rust: the same wrapper as JS sees.
    let h = e.wrap(r, dom.element, HostRef::new(NODE_KIND, 7)).unwrap();
    let f = e
        .evaluate_handle(r, "(n) => n === makeNode(7) && n.expando", origin())
        .unwrap();
    let out = e.call(r, f, Arg::Undefined, &[Arg::Handle(h)]).unwrap();
    assert_eq!(out, JsValue::Number(1.0));
    assert_eq!(
        e.unwrap_host(r, h).unwrap(),
        Some(HostRef::new(NODE_KIND, 7))
    );

    // After the host object dies, a new wrapper is made.
    e.forget_host(HostRef::new(NODE_KIND, 7));
    assert_eq!(eval(&mut e, r, "makeNode(7).expando"), JsValue::Undefined);
}

#[test]
fn interface_shape_matches_webidl() {
    let mut e = engine();
    let dom = define_dom(&mut e);
    let r = e.new_realm(RealmOptions::with_global(dom.window)).unwrap();
    eval(
        &mut e,
        r,
        "var desc = (o, k) => { const d = Object.getOwnPropertyDescriptor(o, k); \
         return [d.writable, d.enumerable, d.configurable, \
         'value' in d ? typeof d.value : 'accessor'].join(); }",
    );
    let checks = [
        ("Node.name", JsValue::String("Node".into())),
        ("Node.length", JsValue::Number(0.0)),
        ("typeof Node", JsValue::String("function".into())),
        (
            "Function.prototype.toString.call(Node)",
            JsValue::String("function Node() { [native code] }".into()),
        ),
        (
            "Object.getPrototypeOf(Element) === Node",
            JsValue::Bool(true),
        ),
        (
            "Object.getPrototypeOf(Element.prototype) === Node.prototype",
            JsValue::Bool(true),
        ),
        (
            "Element.prototype.constructor === Element",
            JsValue::Bool(true),
        ),
        (
            "Object.prototype.toString.call(makeNode(1))",
            JsValue::String("[object Element]".into()),
        ),
        (
            "Object.prototype.toString.call(Node.prototype)",
            JsValue::String("[object Node]".into()),
        ),
        (
            "desc(Node, 'prototype')",
            JsValue::String("false,false,false,object".into()),
        ),
        (
            "desc(Node.prototype, Symbol.toStringTag)",
            JsValue::String("false,false,true,string".into()),
        ),
        (
            "Node.prototype[Symbol.toStringTag]",
            JsValue::String("Node".into()),
        ),
        (
            "desc(Node, 'ELEMENT_NODE')",
            JsValue::String("false,true,false,number".into()),
        ),
        (
            "desc(Node.prototype, 'describe')",
            JsValue::String("true,true,true,function".into()),
        ),
        (
            "desc(Node.prototype, 'nodeId')",
            JsValue::String(",true,true,accessor".into()),
        ),
        (
            "desc(globalThis, 'Node')",
            JsValue::String("true,false,true,function".into()),
        ),
        (
            "desc(globalThis, 'makeNode')",
            JsValue::String("true,true,true,function".into()),
        ),
        (
            "var names = Object.getOwnPropertyNames(globalThis); \
             ['Node', 'Element', 'makeNode', 'nodeIdOf'].map(k => names.indexOf(k)) \
             .every((v, i, a) => v > names.indexOf('Object') && (i == 0 || v > a[i - 1]))",
            JsValue::Bool(true),
        ),
        (
            "Object.getOwnPropertyNames(Node.prototype).join()",
            // Same as Chrome: template members first, `constructor` last.
            JsValue::String("ELEMENT_NODE,nodeId,describe,constructor".into()),
        ),
        (
            "Object.getOwnPropertyDescriptor(Node.prototype, 'nodeId').get.name",
            JsValue::String("get nodeId".into()),
        ),
        (
            "Node.prototype.describe.name",
            JsValue::String("describe".into()),
        ),
        ("Node.prototype.describe.length", JsValue::Number(1.0)),
        (
            "try { Node() } catch (e) { e.message }",
            JsValue::String("Illegal constructor".into()),
        ),
        (
            "try { new Node() } catch (e) { e.message }",
            JsValue::String("Illegal constructor".into()),
        ),
        (
            "try { Node.prototype.describe.call({}) } catch (e) { e.message }",
            JsValue::String("Illegal invocation".into()),
        ),
        (
            "try { Object.getOwnPropertyDescriptor(Node.prototype, 'nodeId').get.call({}) } \
             catch (e) { e.message }",
            JsValue::String("Illegal invocation".into()),
        ),
    ];
    for (src, want) in checks {
        assert_eq!(eval(&mut e, r, src), want, "{src}");
    }
}

#[test]
fn prototype_method_sees_host_handle() {
    let mut e = engine();
    let dom = define_dom(&mut e);
    let r = e.new_realm(RealmOptions::with_global(dom.window)).unwrap();
    assert_eq!(
        eval(&mut e, r, "makeNode(5).describe('x')"),
        JsValue::String("1:5:x".into())
    );
    assert_eq!(eval(&mut e, r, "makeNode(5).nodeId"), JsValue::Number(5.0));
    assert_eq!(
        eval(&mut e, r, "makeNode(5) instanceof Node"),
        JsValue::Bool(true)
    );
    // Wrapping as the base interface still returns the cached wrapper.
    let h = e.wrap(r, dom.node, HostRef::new(NODE_KIND, 5)).unwrap();
    let f = e
        .evaluate_handle(r, "(n) => n === makeNode(5)", origin())
        .unwrap();
    assert_eq!(
        e.call(r, f, Arg::Undefined, &[Arg::Handle(h)]).unwrap(),
        JsValue::Bool(true)
    );
}

#[test]
fn constructor_binds_host_object() {
    let mut e = engine();
    let next = Rc::new(Cell::new(100u32));
    let n = next.clone();
    let event = e.define_interface(
        InterfaceSpec::new("Event")
            .constructor(
                1,
                host_fn(move |cx| {
                    let _type = cx.arg_string(0)?;
                    n.set(n.get() + 1);
                    cx.init_this(HostRef::new(2, n.get()))
                }),
            )
            .prototype(vec![Member::getter(
                "id",
                host_fn(|cx| {
                    let h = cx.this_host().ok_or(HostError::Thrown)?;
                    cx.ret(h.id)
                }),
            )]),
    );
    let window = e.define_interface(
        InterfaceSpec::new("Window").instance(vec![Member::interface("Event", event)]),
    );
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    assert_eq!(eval(&mut e, r, "new Event('x').id"), JsValue::Number(101.0));
    assert_eq!(eval(&mut e, r, "Event.length"), JsValue::Number(1.0));
    assert_eq!(
        eval(&mut e, r, "try { Event('x') } catch (e) { e.message }"),
        JsValue::String(
            "Failed to construct 'Event': Please use the 'new' operator, this DOM object \
             constructor cannot be called as a function."
                .into()
        )
    );
    // The constructed object is the cached wrapper of its host ref.
    let h = e.wrap(r, event, HostRef::new(2, 101)).unwrap();
    assert_eq!(e.get_value(r, h).unwrap(), JsValue::Json("{}".into()));
    let f = e.evaluate_handle(r, "(a) => a.id", origin()).unwrap();
    assert_eq!(
        e.call(r, f, Arg::Undefined, &[Arg::Handle(h)]).unwrap(),
        JsValue::Number(101.0)
    );
}

#[test]
fn global_host_and_named_getters() {
    let mut e = engine();
    let window = e.define_interface(
        InterfaceSpec::new("Window")
            .instance(vec![Member::method(
                "whoami",
                0,
                host_fn(|cx| {
                    let h = cx.this_host().ok_or(HostError::Thrown)?;
                    cx.ret(h.id)
                }),
            )])
            .named_getter(Rc::new(|_cx, name| {
                Ok((name == "magic").then_some(Arg::Number(42.0)))
            }))
            .indexed_getter(Rc::new(|_cx, i| {
                Ok((i < 2).then(|| Arg::from(format!("f{i}"))))
            })),
    );
    let r = e
        .new_realm(RealmOptions {
            global_host: Some(HostRef::new(9, 1)),
            ..RealmOptions::with_global(window)
        })
        .unwrap();
    assert_eq!(eval(&mut e, r, "whoami()"), JsValue::Number(1.0));
    assert_eq!(eval(&mut e, r, "globalThis.whoami()"), JsValue::Number(1.0));
    assert_eq!(eval(&mut e, r, "magic"), JsValue::Number(42.0));
    assert_eq!(
        eval(&mut e, r, "typeof other"),
        JsValue::String("undefined".into())
    );
    assert_eq!(
        eval(&mut e, r, "globalThis[1]"),
        JsValue::String("f1".into())
    );
    assert_eq!(eval(&mut e, r, "globalThis[2]"), JsValue::Undefined);
    let g = e.wrap(r, window, HostRef::new(9, 1)).unwrap();
    let f = e
        .evaluate_handle(r, "(g) => g === globalThis", origin())
        .unwrap();
    assert_eq!(
        e.call(r, f, Arg::Undefined, &[Arg::Handle(g)]).unwrap(),
        JsValue::Bool(true)
    );
}

#[test]
fn calls_js_from_rust_with_exceptions() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    let f = e
        .evaluate_handle(r, "(function (a, b) { return this.k + a + b.n })", origin())
        .unwrap();
    let out = e
        .call(
            r,
            f,
            Arg::Json(r#"{"k":1}"#.into()),
            &[Arg::Number(2.0), Arg::Json(r#"{"n":3}"#.into())],
        )
        .unwrap();
    assert_eq!(out, JsValue::Number(6.0));

    let thrower = e
        .evaluate_handle(
            r,
            "function listener() { undefinedFn(); }\nlistener",
            origin(),
        )
        .unwrap();
    let err = e.call(r, thrower, Arg::Undefined, &[]).unwrap_err();
    assert_eq!(err.message, "undefinedFn is not defined");
    let stack = err.stack.unwrap();
    assert_eq!(
        stack.lines().next(),
        Some("ReferenceError: undefinedFn is not defined")
    );
    assert_eq!(
        stack.lines().nth(1),
        Some("    at listener (https://a.test/x.js:1:23)")
    );

    // Released handles are detected.
    e.release(f);
    let err = e.call(r, f, Arg::Undefined, &[]).unwrap_err();
    assert_eq!(err.kind, JsErrorKind::Host);
}

#[test]
fn microtasks_run_only_at_checkpoint() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    eval(
        &mut e,
        r,
        "var log = []; Promise.resolve().then(() => log.push('a')); log.push('sync');",
    );
    assert_eq!(
        eval(&mut e, r, "log.join()"),
        JsValue::String("sync".into())
    );
    assert_eq!(
        eval(&mut e, r, "log.join()"),
        JsValue::String("sync".into())
    );
    e.perform_microtask_checkpoint();
    assert_eq!(
        eval(&mut e, r, "log.join()"),
        JsValue::String("sync,a".into())
    );
}

#[test]
fn promises_from_rust() {
    let mut e = engine();
    let pending: Rc<RefCell<Vec<lb_js::Resolver>>> = Rc::default();
    let p = pending.clone();
    let window = e.define_interface(InterfaceSpec::new("Window").instance(vec![Member::method(
        "later",
        0,
        host_fn(move |cx| {
            let r = cx.ret_new_promise()?;
            p.borrow_mut().push(r);
            Ok(())
        }),
    )]));
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    eval(
        &mut e,
        r,
        "var out = []; later().then(v => out.push(v)); later().catch(e => out.push(e.message));",
    );
    let rs = pending.borrow_mut().split_off(0);
    assert_eq!(rs.len(), 2);
    e.resolve_promise(rs[0], Arg::from("ok")).unwrap();
    e.reject_promise(rs[1], HostError::type_error("Failed to fetch"))
        .unwrap();
    assert_eq!(eval(&mut e, r, "out.length"), JsValue::Number(0.0));
    e.perform_microtask_checkpoint();
    assert_eq!(
        eval(&mut e, r, "out.join()"),
        JsValue::String("ok,Failed to fetch".into())
    );
    // Settling twice is a host error, not a crash.
    assert!(e.resolve_promise(rs[0], Arg::Undefined).is_err());

    let (promise, resolver) = e.new_promise(r).unwrap();
    let f = e
        .evaluate_handle(r, "(p) => p.then(v => out.push(v * 2))", origin())
        .unwrap();
    e.call(r, f, Arg::Undefined, &[Arg::Handle(promise)])
        .unwrap();
    e.resolve_promise(resolver, Arg::Number(21.0)).unwrap();
    e.perform_microtask_checkpoint();
    assert_eq!(eval(&mut e, r, "out.at(-1)"), JsValue::Number(42.0));
}

const RUNAWAY: &str = "(() => { const a = []; while (true) a.push(new Array(1e5).fill(1.5)); })()";

#[test]
fn heap_limit_callback_fires_on_runaway_allocation() {
    let mut e = Engine::new(
        Arc::new(Persona::default_chrome()),
        EngineOptions {
            max_heap_bytes: Some(32 << 20),
        },
    )
    .unwrap();
    let r = plain_realm(&mut e);
    let err = e.evaluate(r, RUNAWAY, origin()).unwrap_err();
    assert_eq!(err.kind, JsErrorKind::OutOfMemory);
    let hits = e.heap_stats().limit_hits;
    assert!(hits >= 1);
    // The engine stays usable after the garbage is collected.
    e.low_memory_notification();
    assert_eq!(eval(&mut e, r, "1 + 1"), JsValue::Number(2.0));
    // The cap is re-armed: a second runaway is caught the same way.
    let err = e.evaluate(r, RUNAWAY, origin()).unwrap_err();
    assert_eq!(err.kind, JsErrorKind::OutOfMemory);
    assert!(e.heap_stats().limit_hits > hits);
    assert!(e.heap_stats().limit < 64 << 20);
    e.low_memory_notification();
    assert_eq!(eval(&mut e, r, "'ok'"), JsValue::String("ok".into()));
}

#[test]
fn terminate_from_another_thread() {
    let mut e = engine();
    let r = plain_realm(&mut e);
    let h = e.terminate_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(50));
        h.terminate();
    });
    let err = e.evaluate(r, "while (true) {}", origin()).unwrap_err();
    t.join().unwrap();
    assert_eq!(err.kind, JsErrorKind::Terminated);
    assert_eq!(eval(&mut e, r, "'alive'"), JsValue::String("alive".into()));
}

#[test]
fn realms_are_isolated() {
    let mut e = engine();
    let dom = define_dom(&mut e);
    let a = e.new_realm(RealmOptions::with_global(dom.window)).unwrap();
    let b = e
        .new_realm(RealmOptions {
            world: World::Isolated("utility".into()),
            ..RealmOptions::with_global(dom.window)
        })
        .unwrap();
    assert_eq!(e.realm_world(b), Some(World::Isolated("utility".into())));
    eval(
        &mut e,
        a,
        "var secret = 1; Object.prototype.leak = 2; Node.prototype.patched = 3;",
    );
    assert_eq!(
        eval(&mut e, b, "typeof secret"),
        JsValue::String("undefined".into())
    );
    assert_eq!(eval(&mut e, b, "({}).leak"), JsValue::Undefined);
    assert_eq!(eval(&mut e, b, "makeNode(1).patched"), JsValue::Undefined);
    // Same host object, different wrappers per realm.
    eval(&mut e, a, "makeNode(1).tag = 'a'");
    assert_eq!(eval(&mut e, b, "makeNode(1).tag"), JsValue::Undefined);
    assert_eq!(eval(&mut e, b, "makeNode(1).nodeId"), JsValue::Number(1.0));
    assert_eq!(
        eval(&mut e, a, "makeNode(1).tag"),
        JsValue::String("a".into())
    );

    // Dropping a realm frees its handles.
    let h = e.evaluate_handle(b, "({})", origin()).unwrap();
    e.drop_realm(b);
    assert!(e.evaluate(b, "1", origin()).is_err());
    assert!(e.get_value(a, h).is_err());
    assert_eq!(eval(&mut e, a, "secret"), JsValue::Number(1.0));
}

#[test]
fn two_engines_on_one_thread() {
    let mut a = engine();
    let mut b = engine();
    let ra = plain_realm(&mut a);
    let rb = plain_realm(&mut b);
    assert_eq!(
        eval(&mut a, ra, "var x = 'a'; x"),
        JsValue::String("a".into())
    );
    assert_eq!(
        eval(&mut b, rb, "typeof x"),
        JsValue::String("undefined".into())
    );
    // Drop in creation order (not LIFO).
    drop(a);
    assert_eq!(eval(&mut b, rb, "2"), JsValue::Number(2.0));
}

#[test]
fn engines_on_worker_threads() {
    let threads: Vec<_> = (0..3)
        .map(|i| {
            std::thread::spawn(move || {
                let mut e = engine();
                let r = plain_realm(&mut e);
                eval(&mut e, r, &format!("{i} * 10"))
            })
        })
        .collect();
    for (i, t) in threads.into_iter().enumerate() {
        assert_eq!(t.join().unwrap(), JsValue::Number(i as f64 * 10.0));
    }
}

#[test]
fn es_modules_with_loader() {
    let mut e = engine();
    let loads = Rc::new(RefCell::new(Vec::new()));
    let l = loads.clone();
    let loader: lb_js::ModuleLoader = Rc::new(move |spec: &str, referrer: &str| {
        l.borrow_mut().push(format!("{spec} from {referrer}"));
        match spec {
            "./dep.js" => Ok(ModuleSource {
                url: "https://a.test/dep.js".into(),
                source: "export const v = 41; export function boom() { null.y }".into(),
            }),
            _ => Err(format!("Failed to resolve module specifier \"{spec}\"")),
        }
    });
    let r = e
        .new_realm(RealmOptions {
            module_loader: Some(loader),
            ..RealmOptions::default()
        })
        .unwrap();
    let ns = e
        .evaluate_module(
            r,
            "import { v } from './dep.js';\nexport const answer = v + 1;",
            ScriptOrigin::new("https://a.test/main.js"),
        )
        .unwrap();
    assert_eq!(
        e.get_value(r, ns).unwrap(),
        JsValue::Json(r#"{"answer":42}"#.into())
    );
    assert_eq!(
        *loads.borrow(),
        vec!["./dep.js from https://a.test/main.js".to_owned()]
    );

    // A second importer reuses the module map entry.
    let ns2 = e
        .evaluate_module(
            r,
            "import { boom } from './dep.js'; boom();",
            ScriptOrigin::new("https://a.test/two.js"),
        )
        .unwrap_err();
    assert_eq!(ns2.message, "Cannot read properties of null (reading 'y')");
    assert!(
        ns2.stack
            .unwrap()
            .contains("at boom (https://a.test/dep.js:1:")
    );

    let err = e
        .evaluate_module(
            r,
            "import './missing.js';",
            ScriptOrigin::new("https://a.test/m3.js"),
        )
        .unwrap_err();
    assert_eq!(
        err.message,
        "Failed to resolve module specifier \"./missing.js\""
    );

    let p = e.get_property(r, ns, "answer").unwrap();
    assert_eq!(e.get_value(r, p).unwrap(), JsValue::Number(42.0));
}

/// Additions for the WebIDL installer: method aliases, namespace objects,
/// `Arg::Construct`, private values on `this`, self-referencing global and
/// globals with more members than one V8 template holds.
#[test]
fn installer_support() {
    use lb_js::{PropAttrs, WellKnownSymbol};
    let mut e = engine();
    let entries = host_fn(|cx| cx.ret("entries"));
    let pair = e.define_interface(
        InterfaceSpec::new("Pair")
            .constructor(
                1,
                host_fn(|cx| {
                    let label = cx.arg_string(0)?;
                    cx.set_this_private("test#label", label)
                }),
            )
            .prototype(vec![
                Member::method("entries", 0, entries),
                Member::getter(
                    "label",
                    host_fn(|cx| {
                        let v = cx.this_private("test#label");
                        cx.ret(v)
                    }),
                ),
                Member::alias(WellKnownSymbol::Iterator, "entries").with_attrs(PropAttrs {
                    writable: true,
                    enumerable: false,
                    configurable: true,
                }),
            ]),
    );
    let window_id = e.next_interface_id();
    let mut instance = vec![
        Member::interface("Window", window_id),
        Member::interface("Pair", pair),
        Member::namespace(
            "NS",
            "NS",
            vec![Member::method(
                "twice",
                1,
                host_fn(|cx| {
                    let n = cx.arg_f64(0)?;
                    cx.ret(n * 2.0)
                }),
            )],
        ),
        Member::method(
            "makePair",
            1,
            host_fn(move |cx| {
                let s = cx.arg_string(0)?;
                cx.ret(Arg::Construct(pair, vec![Arg::String(s)]))
            }),
        ),
    ];
    // Push past the per-template limit; the tail is defined after creation.
    for i in 0..1100 {
        instance.push(Member::getter(
            format!("g{i}"),
            host_fn(move |cx| cx.ret(i)),
        ));
    }
    let window = e.define_interface(InterfaceSpec::new("Window").instance(instance));
    assert_eq!(window, window_id);
    let r = e.new_realm(RealmOptions::with_global(window)).unwrap();
    let checks: [(&str, JsValue); 9] = [
        (
            "Pair.prototype[Symbol.iterator] === Pair.prototype.entries",
            JsValue::Bool(true),
        ),
        (
            "Window === Object.getPrototypeOf(globalThis).constructor",
            JsValue::Bool(true),
        ),
        (
            "Object.prototype.toString.call(NS) + typeof NS",
            JsValue::String("[object NS]object".into()),
        ),
        ("NS.twice(21)", JsValue::Number(42.0)),
        (
            "Function.prototype.toString.call(NS.twice)",
            JsValue::String("function twice() { [native code] }".into()),
        ),
        (
            "makePair('a').label + new Pair('b').label",
            JsValue::String("ab".into()),
        ),
        (
            "makePair('a') instanceof Pair && makePair('a') !== makePair('a')",
            JsValue::Bool(true),
        ),
        ("g0 + g1099", JsValue::Number(1099.0)),
        (
            // In member order (V8's late globals may sit before the tail).
            "Object.getOwnPropertyNames(globalThis).filter(k => /^g\\d+$/.test(k)) \
             .every((k, i) => k === 'g' + i)",
            JsValue::Bool(true),
        ),
    ];
    for (src, want) in checks {
        assert_eq!(eval(&mut e, r, src), want, "{src}");
    }
}

/// A host function builds a realm on the spot (an iframe's `about:blank`)
/// and hands its global back; same-origin realms share a security token.
#[test]
fn realm_ops_inside_a_callback() {
    use lb_js::RealmOps;
    let mut e = engine();
    let main = e.new_realm(RealmOptions::default()).unwrap();
    e.set_security_token(main, "https://a.test").unwrap();
    let f = e
        .new_function(
            main,
            "makeFrame",
            0,
            host_fn(|cx| {
                let child = cx
                    .new_realm(RealmOptions::default())
                    .map_err(|e| HostError::Error(e.message))?;
                cx.set_security_token(child, "https://a.test")
                    .map_err(|e| HostError::Error(e.message))?;
                RealmOps::evaluate(
                    cx,
                    child,
                    "var inner = 41 + 1;",
                    ScriptOrigin::new("about:blank"),
                )
                .map_err(|e| HostError::Error(e.message))?;
                let g = RealmOps::global(cx, child).map_err(|e| HostError::Error(e.message))?;
                cx.ret(Arg::Handle(g))
            }),
        )
        .unwrap();
    let g = e.global(main).unwrap();
    e.set_property(main, g, "makeFrame", Arg::Handle(f))
        .unwrap();
    let v = e
        .evaluate(
            main,
            "const w = makeFrame(); [w.inner, w !== globalThis, typeof w.Object, w.Object !== Object].join()",
            ScriptOrigin::new("https://a.test/"),
        )
        .unwrap();
    assert_eq!(v, JsValue::String("42,true,function,true".into()));
    // Another origin's global is out of reach.
    let f2 = e
        .new_function(
            main,
            "makeOther",
            0,
            host_fn(|cx| {
                let child = cx
                    .new_realm(RealmOptions::default())
                    .map_err(|e| HostError::Error(e.message))?;
                cx.set_security_token(child, "https://b.test")
                    .map_err(|e| HostError::Error(e.message))?;
                let g = RealmOps::global(cx, child).map_err(|e| HostError::Error(e.message))?;
                cx.ret(Arg::Handle(g))
            }),
        )
        .unwrap();
    e.set_property(main, g, "makeOther", Arg::Handle(f2))
        .unwrap();
    let v = e
        .evaluate(
            main,
            "try { makeOther().Object; 'reached' } catch (e) { e.constructor.name }",
            ScriptOrigin::new("https://a.test/"),
        )
        .unwrap();
    assert_eq!(v, JsValue::String("TypeError".into()));
}

// Large scripts compiled again come from a V8 code cache (run_script):
// results, function source and error positions are the same each time.
#[test]
fn code_cached_scripts_behave_the_same() {
    let mut e = engine();
    // > 16 KB, with a lazily compiled function and a late error position.
    let filler = "// padding line for the code cache threshold\n".repeat(400);
    let src = format!(
        "{filler}function f(a) {{ return a * 2 + {}; }}\n\
         var out = [f(20), String(f).length, typeof g];\n\
         try {{ null.x }} catch (err) {{ out.push(String(err.stack).split('\\n')[1].trim()) }}\n\
         JSON.stringify(out)",
        2
    );
    assert!(src.len() > 16 * 1024);
    let mut seen = Vec::new();
    for _ in 0..4 {
        let r = plain_realm(&mut e);
        seen.push(
            e.evaluate(r, &src, ScriptOrigin::new("https://a.test/big.js"))
                .unwrap(),
        );
        e.drop_realm(r);
    }
    assert!(seen.windows(2).all(|w| w[0] == w[1]), "{seen:?}");
    match &seen[0] {
        JsValue::String(s) => {
            assert!(s.starts_with("[42,"), "{s}");
            assert!(s.ends_with("big.js:403:12\"]"), "{s}");
        }
        v => panic!("{v:?}"),
    }
}

// ICU data shipped compressed (ADR 0013): the prebuilt V8 archive's data,
// byte for byte, and ICU uses it.
mod icu_data {
    use super::*;
    use lb_js::icu_data;
    #[test]
    fn compressed_data_is_the_archives_data() {
        let exe = std::env::current_exe().unwrap();
        let archive = icu_data::find_archive(&exe).unwrap_or_else(|| {
            panic!(
                "librusty_v8.a not found above {} (set RUSTY_V8_ARCHIVE to its path)",
                exe.display()
            )
        });
        let ar = std::fs::read(&archive).unwrap();
        let expected = icu_data::extract_from_archive(&ar).unwrap();
        let data = icu_data::data();
        assert_eq!(
            data.len(),
            expected.len(),
            "length vs {}",
            archive.display()
        );
        assert!(
            data == &expected[..],
            "icu/icudt78.dat.zch is stale; run `cargo run --release -p lb-js --example icu_pack`"
        );
        assert_eq!(
            data.as_ptr() as usize % 16,
            0,
            "ICU wants 16-byte alignment"
        );
    }

    // Lazy paging (Linux): chunks read from many threads at once, in a
    // scattered order, match the eager decompression byte for byte.
    #[test]
    fn lazy_chunks_match_from_many_threads() {
        let data = icu_data::data();
        let eager = icu_data::decompress_all();
        // SAFETY: the box holds at least data.len() initialised bytes.
        let eager = unsafe { std::slice::from_raw_parts(eager.as_ptr().cast::<u8>(), data.len()) };
        #[cfg(target_os = "linux")]
        assert!(
            icu_data::resident_chunks().is_some(),
            "lazy ICU paging is off"
        );
        let chunks = data.len().div_ceil(icu_data::CHUNK);
        std::thread::scope(|s| {
            for t in 0..8 {
                s.spawn(move || {
                    for k in 0..chunks {
                        let i = (k * 7 + t * 13) % chunks;
                        let at = i * icu_data::CHUNK
                            + (t * 4099) % icu_data::CHUNK.min(data.len() - i * icu_data::CHUNK);
                        assert_eq!(data[at], eager[at], "byte {at}");
                    }
                });
            }
        });
        assert!(data == eager);
        #[cfg(target_os = "linux")]
        assert_eq!(icu_data::resident_chunks(), Some(chunks));
    }

    // A fault outside the ICU data is still a crash (the handler passes it
    // on): run in a child process.
    #[cfg(target_os = "linux")]
    #[test]
    fn other_faults_still_crash() {
        if std::env::var_os("LB_ICU_CRASH_CHILD").is_some() {
            let _ = icu_data::data();
            // SAFETY: deliberately invalid; this child process must die.
            unsafe { std::ptr::read_volatile(8usize as *const u8) };
            std::process::exit(0);
        }
        let out = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "icu_data::other_faults_still_crash",
                "--nocapture",
            ])
            .env("LB_ICU_CRASH_CHILD", "1")
            .output()
            .unwrap();
        use std::os::unix::process::ExitStatusExt;
        assert_eq!(out.status.signal(), Some(libc_sigsegv()), "{out:?}");
    }

    #[cfg(target_os = "linux")]
    fn libc_sigsegv() -> i32 {
        11
    }

    #[test]
    fn empty_package_matches_the_real_header() {
        let data = icu_data::data();
        let stub = icu_data::empty_package_bytes();
        // magic, then UDataInfo (size, endianness, charset, UChar size, format
        // "CmnD" and its versions).
        assert_eq!(&stub[2..24], &data[2..24]);
        // An empty table of contents right after the 32-byte header.
        assert_eq!(u16::from_le_bytes([stub[0], stub[1]]), 32);
        assert_eq!(&stub[32..36], &[0, 0, 0, 0]);
    }

    #[test]
    fn intl_uses_the_installed_data() {
        let mut e = engine();
        let r = plain_realm(&mut e);
        let v = e
            .evaluate(
                r,
                "[new Intl.DateTimeFormat('de-DE', {timeZone: 'UTC'}).format(0), \
                  (1234.5).toLocaleString('fr'), \
                  ['ä', 'z', 'a'].sort(new Intl.Collator('sv').compare).join(''), \
                  new Intl.DisplayNames(['ja'], {type: 'region'}).of('US'), \
                  Intl.DateTimeFormat.supportedLocalesOf(['th', 'xx-YY', 'zh-Hant']).join(), \
                  [...new Intl.Segmenter('en', {granularity: 'word'}).segment('a b')].length].join('|')",
                ScriptOrigin::new("x.js"),
            )
            .unwrap();
        assert_eq!(
            v,
            JsValue::String("1.1.1970|1\u{202f}234,5|azä|アメリカ合衆国|th,zh-Hant|3".into())
        );
    }
}

// V8 code that is not linked (`unused_v8`, ADR 0013): the stubs name real
// symbols of the archive, and default V8 never reaches them.
mod unused_v8 {
    use super::*;
    use lb_js::{icu_data, unused_v8};

    /// The global symbol names of the archive's index (GNU `/` member).
    fn archive_symbols() -> std::collections::HashSet<Vec<u8>> {
        let exe = std::env::current_exe().unwrap();
        let archive = icu_data::find_archive(&exe).expect("librusty_v8.a (RUSTY_V8_ARCHIVE)");
        let ar = std::fs::read(&archive).unwrap();
        assert!(ar.starts_with(b"!<arch>\n"));
        let hdr = &ar[8..68];
        assert_eq!(&hdr[..2], b"/ ", "first member is the symbol index");
        let size: usize = std::str::from_utf8(&hdr[48..58])
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let body = &ar[68..68 + size];
        let count = u32::from_be_bytes(body[..4].try_into().unwrap()) as usize;
        body[4 + 4 * count..]
            .split(|&b| b == 0)
            .map(<[u8]>::to_vec)
            .collect()
    }

    #[test]
    fn stubs_name_archive_symbols() {
        let syms = archive_symbols();
        for (path, names) in unused_v8::STUBS {
            for n in *names {
                assert!(
                    syms.contains(n.as_bytes()),
                    "{path}: {n} is not in the V8 archive (renamed? update unused_v8.rs)"
                );
            }
        }
    }

    #[test]
    fn optimizing_tiers_and_wasm_run() {
        let mut e = engine();
        let r = plain_realm(&mut e);
        // Hot JS (Sparkplug, Maglev, Turbofan with OSR), a JS-to-Wasm call
        // in a hot loop (Liftoff, then optimized Wasm), regexps, JSON.
        let v = eval(
            &mut e,
            r,
            r#"
            const bytes = new Uint8Array([0,97,115,109,1,0,0,0,1,7,1,96,2,127,127,1,127,
              3,2,1,0,7,7,1,3,97,100,100,0,0,10,9,1,7,0,32,0,32,1,106,11]);
            const add = new WebAssembly.Instance(new WebAssembly.Module(bytes)).exports.add;
            function step(o, i) { return { x: o.x + (i & 7), y: add(o.y, 1) | 0 }; }
            let o = { x: 0, y: 0 }, hits = 0;
            for (let round = 0; round < 4; round++) {
              for (let i = 0; i < 300000; i++) o = step(o, i);
              for (let i = 0; i < 20000; i++) if (/^(\d+)-([a-z]+)$/.test(i + '-ab')) hits++;
              JSON.parse(JSON.stringify(o));
            }
            [o.x, o.y, hits].join()
            "#,
        );
        assert_eq!(v, JsValue::String("4200000,1200000,80000".into()));
    }
}

/// Interface objects of a global are lazy data properties: they keep their
/// place and look like plain data properties before and after first use,
/// are made in the global's own realm, and get the realm's fixups whichever
/// way they are first reached (global property, wrapper, subclass).
#[test]
fn lazy_interface_objects() {
    let mut e = engine();
    let node = e.define_interface_lazy(Box::new(|| {
        InterfaceSpec::new("Node").prototype(vec![
            Member::method("a", 0, host_fn(|_| Ok(()))),
            Member::method("late", 0, host_fn(|_| Ok(()))),
            Member::method("secret", 0, host_fn(|_| Ok(()))),
        ])
    }));
    let element =
        e.define_interface_lazy(Box::new(move || InterfaceSpec::new("Element").parent(node)));
    let window = e.define_interface(InterfaceSpec::new("Window").instance(vec![
        Member::interface("Node", node),
        Member::method("first", 0, host_fn(|_| Ok(()))),
        Member::interface("Element", element),
        Member::method(
            "makeElement",
            0,
            host_fn(move |cx| cx.ret(Arg::Host(element, HostRef::new(NODE_KIND, 1)))),
        ),
    ]));
    let mut fixups = std::collections::HashMap::new();
    fixups.insert(
        node,
        lb_js::InterfaceFixup {
            proto_late: vec!["late".into()],
            unscopables: vec!["a".into()],
            remove_proto: vec!["secret".into()],
            ..Default::default()
        },
    );
    let fixups: lb_js::InterfaceFixups = Rc::new(fixups);
    let realm = |e: &mut Engine| {
        let r = e
            .new_realm(RealmOptions {
                fixups: Some(fixups.clone()),
                ..RealmOptions::with_global(window)
            })
            .unwrap();
        lb_js::RealmOps::set_security_token(e, r, "https://a.test").unwrap();
        r
    };
    let proto = "Object.getOwnPropertyNames(Node.prototype).join()+'|'+Object.keys(Node.prototype[Symbol.unscopables]).join()";

    // Order and descriptors before any use; the same after.
    let r1 = realm(&mut e);
    let keys = "Object.getOwnPropertyNames(globalThis).filter(k=>/^(Node|first|Element|makeElement)$/.test(k)).join()";
    assert_eq!(
        eval(&mut e, r1, keys),
        JsValue::String("Node,first,Element,makeElement".into())
    );
    let desc = "JSON.stringify(Object.getOwnPropertyDescriptor(globalThis,'Element'),(k,v)=>typeof v==='function'?'fn:'+v.name:v)";
    assert_eq!(
        eval(&mut e, r1, desc),
        JsValue::String(
            r#"{"value":"fn:Element","writable":true,"enumerable":false,"configurable":true}"#
                .into()
        )
    );
    assert_eq!(
        eval(&mut e, r1, keys),
        JsValue::String("Node,first,Element,makeElement".into())
    );
    // Reaching Element made Node first: linked and fixed.
    assert_eq!(
        eval(&mut e, r1, "Object.getPrototypeOf(Element) === Node"),
        JsValue::Bool(true)
    );
    assert_eq!(
        eval(&mut e, r1, proto),
        JsValue::String("a,constructor,late|a".into())
    );

    // Through a wrapper first: the same prototype chain and fixups.
    let r2 = realm(&mut e);
    assert_eq!(
        eval(
            &mut e,
            r2,
            "const x = makeElement(); Object.getPrototypeOf(x) === Element.prototype \
             && Object.getPrototypeOf(Element) === Node && x instanceof Node"
        ),
        JsValue::Bool(true)
    );
    assert_eq!(
        eval(&mut e, r2, proto),
        JsValue::String("a,constructor,late|a".into())
    );

    // Read from another realm: made in the global's own realm.
    let r3 = realm(&mut e);
    let g3 = e.global(r3).unwrap();
    let g1 = e.global(r1).unwrap();
    e.set_property(r1, g1, "other", Arg::Handle(g3)).unwrap();
    assert_eq!(
        eval(
            &mut e,
            r1,
            "other.Node !== Node && Object.getPrototypeOf(other.Element) === other.Node"
        ),
        JsValue::Bool(true)
    );

    // A dropped realm whose global a script still holds keeps its fixups.
    let r4 = realm(&mut e);
    let g4 = e.global(r4).unwrap();
    e.set_property(r1, g1, "gone", Arg::Handle(g4)).unwrap();
    e.drop_realm(r4);
    assert_eq!(
        eval(
            &mut e,
            r1,
            "Object.getOwnPropertyNames(gone.Node.prototype).join()"
        ),
        JsValue::String("a,constructor,late".into())
    );
}

/// Structured clone across two engines (as between a page and a worker):
/// V8's wire format, `ArrayBuffer` transfer (detached at the sender),
/// cycles, and V8's own `DataCloneError` texts.
#[test]
fn structured_clone_between_engines() {
    let mut a = engine();
    let ra = plain_realm(&mut a);
    let v = a
        .evaluate_handle(
            ra,
            "globalThis.buf = new Uint8Array([1,2,3]).buffer; \
             const o = {d: new Date(5), m: new Map([[1, 'x']]), s: new Set([2]), r: /a/g, \
             big: 10n, u8: new Uint8Array(buf), nested: [1, 'two']}; o.self = o; o",
            origin(),
        )
        .unwrap();
    let buf = a.evaluate_handle(ra, "buf", origin()).unwrap();
    let mut no_hosts = |_: HostRef, c: &str| Err(format!("{c} object could not be cloned."));
    let s = a.serialize(ra, v, &[buf], &mut no_hosts).unwrap();
    assert_eq!(s.buffers, vec![vec![1, 2, 3]]);
    assert_eq!(eval(&mut a, ra, "buf.byteLength"), JsValue::Number(0.0));

    let mut b = engine();
    let rb = plain_realm(&mut b);
    let h = b.deserialize(rb, &s, &[]).unwrap();
    let g = b.global(rb).unwrap();
    b.set_property(rb, g, "got", Arg::Handle(h)).unwrap();
    assert_eq!(
        eval(
            &mut b,
            rb,
            "[got.self === got, got.d.getTime(), got.m.get(1), got.s.has(2), String(got.r), \
              typeof got.big, Array.from(got.u8).join(), got.u8.buffer.byteLength, got.nested[1]].join('|')"
        ),
        JsValue::String("true|5|x|true|/a/g|bigint|1,2,3|3|two".into())
    );

    let f = a
        .evaluate_handle(ra, "({f: function foo() {}})", origin())
        .unwrap();
    let err = a.serialize(ra, f, &[], &mut no_hosts).unwrap_err();
    assert_eq!(err.message, "function foo() {} could not be cloned.");
    let sym = a.evaluate_handle(ra, "Symbol('q')", origin()).unwrap();
    let err = a.serialize(ra, sym, &[], &mut no_hosts).unwrap_err();
    assert_eq!(err.message, "Symbol(q) could not be cloned.");
    let detached = a.evaluate_handle(ra, "buf", origin()).unwrap();
    let err = a
        .serialize(ra, detached, &[detached], &mut no_hosts)
        .unwrap_err();
    assert_eq!(err.message, "ArrayBuffer at index 0 is already detached.");
}
