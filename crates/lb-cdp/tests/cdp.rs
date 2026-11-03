//! End-to-end protocol tests: a real server on 127.0.0.1:0 with the mock
//! host, driven over WebSocket the way Playwright drives Chrome.

use std::collections::VecDeque;
use std::rc::Rc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use lb_cdp::mock::MockHost;
use lb_cdp::{Server, ServerConfig, block_on_local};
use lb_persona::PersonaProfile;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_tungstenite::tungstenite::Message;

const WIN: &str = "chrome153_win10_rtx3060_1080p.json";
const MAC: &str = "chrome153_macos15_apple_m2_retina.json";

fn persona_json(name: &str) -> String {
    let path = format!(
        "{}/../../profiles/reference/chrome/{name}",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read_to_string(path).unwrap()
}

fn persona(name: &str) -> PersonaProfile {
    PersonaProfile::from_json(&persona_json(name)).unwrap()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Client {
    ws: Ws,
    next: i64,
    events: VecDeque<Value>,
}

impl Client {
    async fn connect(url: &str) -> Client {
        let (ws, _) = tokio_tungstenite::connect_async(url).await.unwrap();
        Client {
            ws,
            next: 0,
            events: VecDeque::new(),
        }
    }

    async fn recv(&mut self) -> Value {
        let msg = tokio::time::timeout(Duration::from_secs(5), self.ws.next())
            .await
            .expect("timed out waiting for a message")
            .unwrap()
            .unwrap();
        match msg {
            Message::Text(t) => serde_json::from_str(&t).unwrap(),
            other => panic!("unexpected frame {other:?}"),
        }
    }

    async fn send_raw(&mut self, text: &str) {
        self.ws.send(Message::Text(text.into())).await.unwrap();
    }

    /// Sends a command; returns `Ok(result)` or `Err(error)`. Events seen
    /// before the reply are kept, in order.
    async fn call(
        &mut self,
        method: &str,
        params: Value,
        session: Option<&str>,
    ) -> Result<Value, Value> {
        self.next += 1;
        let id = self.next;
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = json!(s);
        }
        self.send_raw(&msg.to_string()).await;
        loop {
            let m = self.recv().await;
            if m.get("id").and_then(Value::as_i64) == Some(id) {
                return match m.get("error") {
                    Some(e) => Err(e.clone()),
                    None => Ok(m["result"].clone()),
                };
            }
            assert!(m.get("id").is_none(), "reply for another id: {m}");
            self.events.push_back(m);
        }
    }

    async fn ok(&mut self, method: &str, params: Value, session: Option<&str>) -> Value {
        match self.call(method, params, session).await {
            Ok(v) => v,
            Err(e) => panic!("{method} failed: {e}"),
        }
    }

    /// Waits for an event (looking at buffered ones first).
    async fn event(&mut self, method: &str, session: Option<&str>) -> Value {
        let matches = |m: &Value| {
            m["method"] == method && m.get("sessionId").and_then(Value::as_str) == session
        };
        if let Some(i) = self.events.iter().position(matches) {
            return self.events.remove(i).unwrap()["params"].clone();
        }
        loop {
            let m = self.recv().await;
            if matches(&m) {
                return m["params"].clone();
            }
            self.events.push_back(m);
        }
    }

    fn seen(&self, method: &str) -> Vec<Value> {
        self.events
            .iter()
            .filter(|m| m["method"] == method)
            .map(|m| m["params"].clone())
            .collect()
    }
}

/// Starts a server with `host`; returns `(ws_url, http_addr)`.
async fn start(host: Rc<MockHost>) -> (String, std::net::SocketAddr) {
    let cfg = ServerConfig {
        addr: "127.0.0.1:0".parse().unwrap(),
        allow_origins: Vec::new(),
    };
    let server = Server::bind(cfg, host).await.unwrap();
    let url = server.browser_ws_url();
    let addr = server.local_addr();
    tokio::task::spawn_local(async move {
        server.run().await.unwrap();
    });
    (url, addr)
}

async fn http(addr: std::net::SocketAddr, req: &str) -> (String, String) {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let (head, body) = out.split_once("\r\n\r\n").unwrap();
    (head.to_string(), body.to_string())
}

/// Browser connection, auto-attach, a new context and a page, initialised
/// like Playwright's FrameSession. Returns `(client, context, target, session)`.
async fn new_page(url: &str) -> (Client, String, String, String) {
    let mut c = Client::connect(url).await;
    c.ok("Browser.getVersion", json!({}), None).await;
    c.ok(
        "Target.setAutoAttach",
        json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
        None,
    )
    .await;
    let ctx = c
        .ok(
            "Target.createBrowserContext",
            json!({"disposeOnDetach": true}),
            None,
        )
        .await["browserContextId"]
        .as_str()
        .unwrap()
        .to_string();
    let t = c
        .ok(
            "Target.createTarget",
            json!({"url": "about:blank", "browserContextId": ctx}),
            None,
        )
        .await["targetId"]
        .as_str()
        .unwrap()
        .to_string();
    // Playwright needs attachedToTarget before the createTarget reply.
    let attached = c.seen("Target.attachedToTarget");
    assert_eq!(attached.len(), 1, "attachedToTarget must precede the reply");
    let a = &attached[0];
    assert_eq!(a["targetInfo"]["targetId"], t.as_str());
    assert_eq!(a["targetInfo"]["type"], "page");
    assert_eq!(a["targetInfo"]["browserContextId"], ctx.as_str());
    assert_eq!(a["waitingForDebugger"], true);
    let s = a["sessionId"].as_str().unwrap().to_string();
    c.events.clear();
    let sid = Some(s.as_str());
    c.ok("Page.enable", json!({}), sid).await;
    let tree = c.ok("Page.getFrameTree", json!({}), sid).await;
    assert_eq!(tree["frameTree"]["frame"]["id"], t.as_str());
    assert_eq!(tree["frameTree"]["frame"]["url"], "about:blank");
    c.ok("Log.enable", json!({}), sid).await;
    c.ok(
        "Page.setLifecycleEventsEnabled",
        json!({"enabled": true}),
        sid,
    )
    .await;
    c.ok("Runtime.enable", json!({}), sid).await;
    let created = c.event("Runtime.executionContextCreated", sid).await;
    assert_eq!(created["context"]["auxData"]["isDefault"], true);
    assert_eq!(created["context"]["auxData"]["frameId"], t.as_str());
    c.ok(
        "Page.addScriptToEvaluateOnNewDocument",
        json!({"source": "", "worldName": "__playwright_utility_world__"}),
        sid,
    )
    .await;
    c.ok("Network.enable", json!({}), sid).await;
    c.ok(
        "Target.setAutoAttach",
        json!({"autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true}),
        sid,
    )
    .await;
    c.ok(
        "Emulation.setFocusEmulationEnabled",
        json!({"enabled": true}),
        sid,
    )
    .await;
    c.ok(
        "Emulation.setEmulatedMedia",
        json!({"media": "", "features": [
            {"name": "prefers-color-scheme", "value": "light"},
            {"name": "prefers-reduced-motion", "value": ""},
            {"name": "forced-colors", "value": ""},
            {"name": "prefers-contrast", "value": ""}
        ]}),
        sid,
    )
    .await;
    c.ok("Runtime.runIfWaitingForDebugger", json!({}), sid)
        .await;
    let world = c
        .ok(
            "Page.createIsolatedWorld",
            json!({"frameId": t, "grantUniveralAccess": true, "worldName": "__playwright_utility_world__"}),
            sid,
        )
        .await;
    let ev = c.event("Runtime.executionContextCreated", sid).await;
    assert_eq!(ev["context"]["id"], world["executionContextId"]);
    assert_eq!(ev["context"]["name"], "__playwright_utility_world__");
    assert_eq!(ev["context"]["auxData"]["isDefault"], false);
    c.events.clear();
    (c, ctx, t, s)
}

/// Navigates and waits for `load`; returns the new contexts by world name.
async fn goto(c: &mut Client, s: &str, url: &str) -> Vec<Value> {
    let sid = Some(s);
    let nav = c.ok("Page.navigate", json!({"url": url}), sid).await;
    assert!(nav.get("errorText").is_none(), "{nav}");
    loop {
        let ev = c.event("Page.lifecycleEvent", sid).await;
        if ev["name"] == "load" {
            assert_eq!(ev["loaderId"], nav["loaderId"]);
            break;
        }
    }
    let nav_ev = c.seen("Page.frameNavigated");
    assert_eq!(nav_ev.last().unwrap()["frame"]["loaderId"], nav["loaderId"]);
    let ctxs: Vec<Value> = c
        .seen("Runtime.executionContextCreated")
        .into_iter()
        .map(|e| e["context"].clone())
        .collect();
    c.events.clear();
    ctxs
}

fn ctx_of(ctxs: &[Value], main: bool) -> i64 {
    ctxs.iter()
        .find(|c| c["auxData"]["isDefault"] == main)
        .unwrap()["id"]
        .as_i64()
        .unwrap()
}

fn host() -> Rc<MockHost> {
    Rc::new(MockHost::new(persona(WIN)))
}

#[test]
fn http_endpoints_and_guards() {
    block_on_local(async {
        let (ws, addr) = start(host()).await;
        let (head, body) = http(addr, "GET /json/version HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
        assert!(head.starts_with("HTTP/1.1 200"), "{head}");
        let v: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["Browser"], "Chrome/153.0.8010.12");
        assert_eq!(v["Protocol-Version"], "1.3");
        assert!(v["User-Agent"].as_str().unwrap().contains("Chrome/153"));
        let dbg = v["webSocketDebuggerUrl"].as_str().unwrap();
        assert!(dbg.starts_with("ws://127.0.0.1/devtools/browser/"));
        assert_eq!(dbg.rsplit('/').next(), ws.rsplit('/').next());

        let (_, body) = http(addr, "GET /json/protocol HTTP/1.1\r\nHost: localhost\r\n\r\n").await;
        let v: Value = serde_json::from_str(&body).unwrap();
        assert!(v["domains"].as_array().unwrap().iter().any(|d| d["domain"] == "Runtime"));

        let (_, body) = http(addr, "PUT /json/new?http://127.0.0.1:9/x HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
        let t: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(t["type"], "page");
        let id = t["id"].as_str().unwrap().to_string();
        let (_, body) = http(addr, "GET /json/list HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").await;
        let list: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(list[0]["id"], id.as_str());
        assert!(list[0]["webSocketDebuggerUrl"].as_str().unwrap().ends_with(&id));
        let (head, _) = http(addr, &format!("GET /json/close/{id} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")).await;
        assert!(head.starts_with("HTTP/1.1 200"));

        // DNS rebinding guard.
        let (head, body) = http(addr, "GET /json/version HTTP/1.1\r\nHost: evil.example\r\n\r\n").await;
        assert!(head.starts_with("HTTP/1.1 500"), "{head}");
        assert!(body.contains("not an IP address or localhost"));
        // WebSockets from web pages (with an Origin) are refused.
        let (head, _) = http(
            addr,
            &format!(
                "GET /devtools/browser/x HTTP/1.1\r\nHost: 127.0.0.1\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\nSec-WebSocket-Version: 13\r\nOrigin: http://127.0.0.1:{}\r\n\r\n",
                addr.port()
            ),
        )
        .await;
        assert!(head.starts_with("HTTP/1.1 403"), "{head}");
    })
    .unwrap();
}

#[test]
fn framing_and_errors() {
    block_on_local(async {
        let (ws, _) = start(host()).await;
        let mut c = Client::connect(&ws).await;
        c.send_raw("{not json").await;
        let m = c.recv().await;
        assert_eq!(m["error"]["code"], -32700);
        c.send_raw(r#"{"id": 7}"#).await;
        let m = c.recv().await;
        assert_eq!(m["id"], 7);
        assert_eq!(m["error"]["code"], -32600);

        let e = c.call("Foo.bar", json!({}), None).await.unwrap_err();
        assert_eq!(
            e,
            json!({"code": -32601, "message": "'Foo.bar' wasn't found"})
        );
        let e = c.call("Page.enable", json!({}), None).await.unwrap_err();
        assert_eq!(e["code"], -32601, "page domain on the browser session");
        let e = c
            .call("Runtime.enable", json!({}), Some("NOPE"))
            .await
            .unwrap_err();
        assert_eq!(e["code"], -32001);
        let e = c
            .call("Target.closeTarget", json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(e["code"], -32602);
        let e = c
            .call("Page.captureScreenshot", json!({}), None)
            .await
            .unwrap_err();
        assert_eq!(e["code"], -32601);
        let e = c
            .call(
                "Target.setAutoAttach",
                json!({"autoAttach": true, "flatten": false}),
                None,
            )
            .await
            .unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("flatten"));
    })
    .unwrap();
}

#[test]
fn every_listed_method_is_dispatched() {
    block_on_local(async {
        let (ws, _) = start(host()).await;
        let (mut c, _, _, s) = new_page(&ws).await;
        for (domain, cmds) in lb_cdp::methods::COMMANDS {
            for cmd in *cmds {
                let method = format!("{domain}.{cmd}");
                if matches!(
                    method.as_str(),
                    "Browser.close"
                        | "Page.close"
                        | "Target.closeTarget"
                        | "Target.detachFromTarget"
                        | "Target.disposeBrowserContext"
                ) {
                    continue;
                }
                let session = if matches!(*domain, "Browser" | "Target" | "Storage") {
                    None
                } else {
                    Some(s.as_str())
                };
                if let Err(e) = c.call(&method, json!({}), session).await {
                    assert_ne!(
                        e["code"], -32601,
                        "{method} is listed but not dispatched: {e}"
                    );
                }
            }
        }
    })
    .unwrap();
}

#[test]
fn page_navigation_runtime_and_isolated_worlds() {
    block_on_local(async {
        let h = host();
        h.add_page(
            "http://127.0.0.1:8765/shop",
            "<!DOCTYPE html><html><head><title>Shop</title></head><body><p id=x class='a b'>Hi</p></body></html>",
        );
        let (ws, _) = start(h.clone()).await;
        let (mut c, _, t, s) = new_page(&ws).await;
        let sid = Some(s.as_str());
        c.ok("Target.setDiscoverTargets", json!({"discover": true}), None).await;
        c.events.clear();

        let nav = c.ok("Page.navigate", json!({"url": "http://127.0.0.1:8765/shop"}), sid).await;
        assert_eq!(nav["frameId"], t.as_str());
        let name_of = |m: &Value| {
            let name = m["method"].as_str().unwrap_or("").to_string();
            if name == "Page.lifecycleEvent" {
                format!("lifecycle:{}", m["params"]["name"].as_str().unwrap())
            } else {
                name
            }
        };
        // Events sent during the command arrive before its reply.
        let mut order: Vec<String> = c.events.iter().map(name_of).collect();
        assert!(order.contains(&"Page.frameNavigated".to_string()), "{order:?}");
        loop {
            let m = c.recv().await;
            let done = m["method"] == "Page.lifecycleEvent" && m["params"]["name"] == "networkIdle";
            order.push(name_of(&m));
            c.events.push_back(m);
            if done {
                break;
            }
        }
        let pos = |n: &str| order.iter().position(|x| x == n).unwrap_or_else(|| panic!("{n} missing: {order:?}"));
        assert!(pos("Runtime.executionContextsCleared") < pos("Page.frameNavigated"));
        assert!(pos("Page.frameNavigated") < pos("Runtime.executionContextCreated"));
        assert!(pos("lifecycle:init") < pos("lifecycle:DOMContentLoaded"));
        assert!(pos("Page.domContentEventFired") < pos("Page.loadEventFired"));
        assert!(pos("lifecycle:load") < pos("Page.frameStoppedLoading"));
        let infos = c.seen("Target.targetInfoChanged");
        assert!(infos.iter().any(|i| i["targetInfo"]["title"] == "Shop"), "{infos:?}");
        // The utility world comes back on its own after navigation.
        let ctxs: Vec<Value> = c.seen("Runtime.executionContextCreated").into_iter().map(|e| e["context"].clone()).collect();
        assert_eq!(ctxs.len(), 2, "{ctxs:?}");
        assert!(ctxs.iter().any(|x| x["name"] == "__playwright_utility_world__"));
        assert_eq!(ctxs[0]["origin"], "http://127.0.0.1:8765");
        let main = ctx_of(&ctxs, true);
        let utility = ctx_of(&ctxs, false);
        c.events.clear();

        // Runtime.evaluate defaults to the main world.
        let r = c.ok("Runtime.evaluate", json!({"expression": "1+1", "returnByValue": true}), sid).await;
        assert_eq!(r["result"], json!({"type": "number", "value": 2, "description": "2"}));
        let r = c.ok("Runtime.evaluate", json!({"expression": "document.title", "contextId": utility}), sid).await;
        assert_eq!(r["result"]["value"], "Shop");
        let r = c.ok("Runtime.evaluate", json!({"expression": "navigator.userAgent"}), sid).await;
        assert!(r["result"]["value"].as_str().unwrap().contains("Windows NT 10.0"));
        let r = c.ok("Runtime.evaluate", json!({"expression": "navigator.webdriver"}), sid).await;
        assert_eq!(r["result"]["value"], false);
        let e = c.call("Runtime.evaluate", json!({"expression": "1", "contextId": 9999}), sid).await.unwrap_err();
        assert_eq!(e["message"], "Cannot find context with specified id");

        // Exceptions come back as exceptionDetails.
        let r = c.ok("Runtime.evaluate", json!({"expression": "throw new Error('boom')"}), sid).await;
        assert_eq!(r["exceptionDetails"]["text"], "Uncaught");
        assert!(r["exceptionDetails"]["exception"]["description"].as_str().unwrap().starts_with("Error: boom"));
        let r = c.ok("Runtime.evaluate", json!({"expression": "missingThing"}), sid).await;
        assert!(r["exceptionDetails"]["exception"]["description"].as_str().unwrap().starts_with("ReferenceError"));

        // Objects, previews, getProperties, callFunctionOn, release.
        let r = c.ok("Runtime.evaluate", json!({"expression": "[1, 'a', [2]]", "generatePreview": true}), sid).await;
        assert_eq!(r["result"]["description"], "Array(3)");
        assert_eq!(r["result"]["subtype"], "array");
        assert_eq!(r["result"]["preview"]["properties"].as_array().unwrap().len(), 3);
        let obj = r["result"]["objectId"].as_str().unwrap().to_string();
        let props = c.ok("Runtime.getProperties", json!({"objectId": obj, "ownProperties": true}), sid).await;
        assert_eq!(props["result"][1]["value"]["value"], "a");
        assert_eq!(props["result"][2]["value"]["description"], "Array(1)");
        let r = c
            .ok(
                "Runtime.callFunctionOn",
                json!({
                    "functionDeclaration": "(a, b) => a + b",
                    "executionContextId": main,
                    "arguments": [{"value": 40}, {"value": 2}],
                    "returnByValue": true
                }),
                sid,
            )
            .await;
        assert_eq!(r["result"]["value"], 42);
        let r = c
            .ok(
                "Runtime.callFunctionOn",
                json!({"functionDeclaration": "function(x) { return x.length; }", "objectId": obj, "arguments": [{"objectId": obj}], "returnByValue": true}),
                sid,
            )
            .await;
        assert_eq!(r["result"]["value"], 3);
        let r = c
            .ok(
                "Runtime.callFunctionOn",
                json!({"functionDeclaration": "(n) => n", "executionContextId": main, "arguments": [{"unserializableValue": "-0"}]}),
                sid,
            )
            .await;
        assert_eq!(r["result"]["unserializableValue"], "-0");
        c.ok("Runtime.releaseObject", json!({"objectId": obj}), sid).await;
        let e = c.call("Runtime.getProperties", json!({"objectId": obj}), sid).await.unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("Could not find object"));
        let e = c.call("Runtime.callFunctionOn", json!({"functionDeclaration": "() => 1"}), sid).await.unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("Either ObjectId or executionContextId"));
        c.ok("Runtime.releaseObjectGroup", json!({"objectGroup": "g"}), sid).await;

        // Console stream and page errors.
        c.ok("Runtime.evaluate", json!({"expression": "console.log('hello', 3)"}), sid).await;
        let ev = c.event("Runtime.consoleAPICalled", sid).await;
        assert_eq!(ev["type"], "log");
        assert_eq!(ev["args"][0]["value"], "hello");
        assert_eq!(ev["args"][1]["value"], 3);
        assert_eq!(ev["executionContextId"], main);
        h.page_error(&t, "Error: late");
        let ev = c.event("Runtime.exceptionThrown", sid).await;
        assert!(ev["exceptionDetails"]["exception"]["description"].as_str().unwrap().contains("late"));

        // Playwright's evaluate path: UtilityScript handle + trampoline.
        let us = c
            .ok(
                "Runtime.evaluate",
                json!({"expression": "(() => { const module = {}; /* src */ return new (module.exports.UtilityScript())(globalThis, false); })();", "contextId": main}),
                sid,
            )
            .await;
        assert_eq!(us["result"]["className"], "UtilityScript");
        let us = us["result"]["objectId"].as_str().unwrap().to_string();
        let call = |fun: bool, expr: &str| {
            json!({
                "functionDeclaration": "(utilityScript, ...args) => utilityScript.evaluate(...args)",
                "objectId": us,
                "arguments": [{"objectId": us}, {"value": fun}, {"value": true}, {"value": expr}, {"value": 0}],
                "returnByValue": true, "awaitPromise": true, "userGesture": true
            })
        };
        let r = c.ok("Runtime.callFunctionOn", call(false, "1+1"), sid).await;
        assert_eq!(r["result"]["value"], 2);
        let content = "() => { let retVal = ''; if (document.doctype) retVal = new XMLSerializer().serializeToString(document.doctype); if (document.documentElement) retVal += document.documentElement.outerHTML; return retVal; }";
        let r = c.ok("Runtime.callFunctionOn", call(true, content), sid).await;
        let html = r["result"]["value"].as_str().unwrap();
        assert!(html.starts_with("<!DOCTYPE html><html><head><title>Shop</title>"), "{html}");
        let r = c.ok("Runtime.callFunctionOn", call(false, "undefined"), sid).await;
        assert_eq!(r["result"]["value"], json!({"v": "undefined"}));

        // Reload keeps the URL and makes new realms.
        c.ok("Page.reload", json!({}), sid).await;
        let ev = c.event("Page.frameNavigated", sid).await;
        assert_eq!(ev["frame"]["url"], "http://127.0.0.1:8765/shop");

        // Non-loopback hosts never load in the mock.
        let r = c.ok("Page.navigate", json!({"url": "https://example.com/"}), sid).await;
        assert_eq!(r["errorText"], "net::ERR_BLOCKED_BY_CLIENT");
        let e = c.call("Page.captureScreenshot", json!({}), sid).await.unwrap_err();
        assert_eq!(e["message"], "Page.captureScreenshot is not supported");
    })
    .unwrap();
}

#[test]
fn bindings_never_reach_the_main_world() {
    block_on_local(async {
        let (ws, _) = start(host()).await;
        let (mut c, _, _, s) = new_page(&ws).await;
        let sid = Some(s.as_str());
        let ctxs = goto(&mut c, &s, "http://127.0.0.1:8765/").await;
        let (main, utility) = (ctx_of(&ctxs, true), ctx_of(&ctxs, false));

        // Playwright's call: no context given -> isolated worlds only.
        c.ok(
            "Runtime.addBinding",
            json!({"name": "__playwright__binding__"}),
            sid,
        )
        .await;
        let r = c
            .ok(
                "Runtime.evaluate",
                json!({"expression": "typeof __playwright__binding__", "contextId": main}),
                sid,
            )
            .await;
        assert_eq!(r["result"]["value"], "undefined");
        let r = c
            .ok(
                "Runtime.evaluate",
                json!({"expression": "__playwright__binding__('x')", "contextId": main}),
                sid,
            )
            .await;
        assert!(
            r["exceptionDetails"].is_object(),
            "main world must not see the binding"
        );
        let r = c
            .ok(
                "Runtime.evaluate",
                json!({"expression": "__playwright__binding__('payload')", "contextId": utility}),
                sid,
            )
            .await;
        assert!(r.get("exceptionDetails").is_none(), "{r}");
        let ev = c.event("Runtime.bindingCalled", sid).await;
        assert_eq!(ev["payload"], "payload");
        assert_eq!(ev["executionContextId"], utility);

        // Asking for the main world explicitly is refused.
        let e = c
            .call(
                "Runtime.addBinding",
                json!({"name": "leak", "executionContextId": main}),
                sid,
            )
            .await
            .unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("isolated worlds"));
        c.ok(
            "Runtime.addBinding",
            json!({"name": "w", "executionContextName": "__playwright_utility_world__"}),
            sid,
        )
        .await;
    })
    .unwrap();
}

#[test]
fn dom_input_and_accessibility() {
    block_on_local(async {
        let h = host();
        let (ws, _) = start(h.clone()).await;
        let (mut c, _, t, s) = new_page(&ws).await;
        let sid = Some(s.as_str());
        let ctxs = goto(&mut c, &s, "http://127.0.0.1:8765/login").await;
        let utility = ctx_of(&ctxs, false);

        c.ok("DOM.enable", json!({}), sid).await;
        let doc = c.ok("DOM.getDocument", json!({"depth": -1}), sid).await;
        assert_eq!(doc["root"]["nodeName"], "#document");
        assert_eq!(doc["root"]["frameId"], t.as_str());
        assert_eq!(doc["root"]["documentURL"], "http://127.0.0.1:8765/login");
        let root = doc["root"]["nodeId"].as_i64().unwrap();
        let user = c
            .ok(
                "DOM.querySelector",
                json!({"nodeId": root, "selector": "#user"}),
                sid,
            )
            .await["nodeId"]
            .as_i64()
            .unwrap();
        assert!(user > 0);
        let none = c
            .ok(
                "DOM.querySelector",
                json!({"nodeId": root, "selector": "#nope"}),
                sid,
            )
            .await;
        assert_eq!(none["nodeId"], 0);
        let all = c
            .ok(
                "DOM.querySelectorAll",
                json!({"nodeId": root, "selector": "input"}),
                sid,
            )
            .await;
        assert_eq!(all["nodeIds"].as_array().unwrap().len(), 2);
        let d = c
            .ok("DOM.describeNode", json!({"backendNodeId": user}), sid)
            .await;
        assert_eq!(d["node"]["nodeName"], "INPUT");
        assert_eq!(
            d["node"]["attributes"],
            json!(["id", "user", "name", "user"])
        );
        let html = c.ok("DOM.getOuterHTML", json!({"nodeId": user}), sid).await;
        assert_eq!(html["outerHTML"], r#"<input id="user" name="user">"#);

        // resolveNode in the utility world, then back with requestNode.
        let obj = c
            .ok(
                "DOM.resolveNode",
                json!({"backendNodeId": user, "executionContextId": utility}),
                sid,
            )
            .await["object"]
            .clone();
        assert_eq!(obj["subtype"], "node");
        assert_eq!(obj["className"], "HTMLInputElement");
        assert_eq!(obj["description"], "input#user");
        let oid = obj["objectId"].as_str().unwrap();
        let back = c.ok("DOM.requestNode", json!({"objectId": oid}), sid).await;
        assert_eq!(back["nodeId"], user);
        let d = c
            .ok("DOM.describeNode", json!({"objectId": oid}), sid)
            .await;
        assert_eq!(d["node"]["backendNodeId"], user);

        // Geometry from lb-layout (Chrome-on-Windows text input 153x21).
        let bm = c.ok("DOM.getBoxModel", json!({"objectId": oid}), sid).await["model"].clone();
        assert_eq!(bm["width"], 153.0);
        assert_eq!(bm["height"], 21.0);
        let quads = c
            .ok("DOM.getContentQuads", json!({"objectId": oid}), sid)
            .await["quads"]
            .clone();
        let q = quads[0].as_array().unwrap();
        c.ok(
            "DOM.scrollIntoViewIfNeeded",
            json!({"objectId": oid, "rect": {"x": 0, "y": 0, "width": 1, "height": 1}}),
            sid,
        )
        .await;
        let e = c
            .call(
                "DOM.setFileInputFiles",
                json!({"objectId": oid, "files": ["/tmp/x"]}),
                sid,
            )
            .await
            .unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("not a file input"));
        let e = c.call("DOM.getBoxModel", json!({}), sid).await.unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("Either nodeId"));

        // Click the input's centre, then type.
        let (x, y) = (
            (q[0].as_f64().unwrap() + q[2].as_f64().unwrap()) / 2.0,
            (q[1].as_f64().unwrap() + q[5].as_f64().unwrap()) / 2.0,
        );
        for kind in ["mouseMoved", "mousePressed", "mouseReleased"] {
            c.ok(
                "Input.dispatchMouseEvent",
                json!({"type": kind, "x": x, "y": y, "button": "left", "clickCount": 1}),
                sid,
            )
            .await;
        }
        c.ok("Input.insertText", json!({"text": "alice"}), sid)
            .await;
        c.ok(
            "Input.dispatchKeyEvent",
            json!({"type": "keyDown", "key": "!", "text": "!"}),
            sid,
        )
        .await;
        c.ok(
            "Input.dispatchKeyEvent",
            json!({"type": "keyUp", "key": "!"}),
            sid,
        )
        .await;
        assert_eq!(h.input_value(&t, "user").as_deref(), Some("alice!"));
        let log = h.input_log();
        assert!(
            log.iter()
                .any(|l| l.starts_with("mousePressed") && l.ends_with("input#user")),
            "{log:?}"
        );
        let e = c
            .call(
                "Input.dispatchTouchEvent",
                json!({"type": "touchStart", "touchPoints": []}),
                sid,
            )
            .await
            .unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("no touch screen"));

        // Accessibility (getByRole runs in the page; these back the CDP API).
        let ax = c.ok("Accessibility.getFullAXTree", json!({}), sid).await["nodes"].clone();
        let nodes = ax.as_array().unwrap();
        assert_eq!(nodes[0]["role"]["value"], "RootWebArea");
        assert!(
            nodes
                .iter()
                .any(|n| n["role"]["value"] == "button" && n["name"]["value"] == "Log in")
        );
        assert!(
            nodes
                .iter()
                .any(|n| n["role"]["value"] == "checkbox" && n["name"]["value"] == "Remember me")
        );
        let q = c
            .ok(
                "Accessibility.queryAXTree",
                json!({"backendNodeId": root, "role": "textbox", "accessibleName": "User"}),
                sid,
            )
            .await;
        assert_eq!(q["nodes"][0]["backendDOMNodeId"], user);
    })
    .unwrap();
}

#[test]
fn network_cookies_and_fetch_interception() {
    block_on_local(async {
        let (ws, _) = start(host()).await;
        let (mut c, ctx, t, s) = new_page(&ws).await;
        let sid = Some(s.as_str());
        let url = "http://127.0.0.1:8765/news";
        let nav = c.ok("Page.navigate", json!({"url": url}), sid).await;
        let req = c.event("Network.requestWillBeSent", sid).await;
        assert_eq!(req["requestId"], nav["loaderId"]);
        assert_eq!(req["loaderId"], nav["loaderId"]);
        assert_eq!(req["type"], "Document");
        assert_eq!(req["frameId"], t.as_str());
        assert_eq!(req["documentURL"], url);
        let resp = c.event("Network.responseReceived", sid).await;
        assert_eq!(resp["response"]["status"], 200);
        let fin = c.event("Network.loadingFinished", sid).await;
        assert_eq!(fin["requestId"], nav["loaderId"]);
        let body = c.ok("Network.getResponseBody", json!({"requestId": nav["loaderId"]}), sid).await;
        assert!(body["body"].as_str().unwrap().contains("Mock page"));
        assert_eq!(body["base64Encoded"], false);
        c.ok("Network.setExtraHTTPHeaders", json!({"headers": {"X-Test": "1"}}), sid).await;
        c.ok("Network.setCacheDisabled", json!({"cacheDisabled": true}), sid).await;
        c.ok("Security.setIgnoreCertificateErrors", json!({"ignore": true}), sid).await;

        // Cookies through Network (page session) and Storage (browser session).
        c.ok(
            "Network.setCookies",
            json!({"cookies": [{"name": "a", "value": "1", "url": "http://127.0.0.1:8765/"},
                               {"name": "b", "value": "2", "domain": "localhost", "path": "/x"}]}),
            sid,
        )
        .await;
        let got = c.ok("Network.getCookies", json!({"urls": [url]}), sid).await;
        let names: Vec<&str> = got["cookies"].as_array().unwrap().iter().map(|c| c["name"].as_str().unwrap()).collect();
        assert_eq!(names, ["a"]);
        assert_eq!(got["cookies"][0]["session"], true);
        let got = c.ok("Storage.getCookies", json!({"browserContextId": ctx}), None).await;
        assert_eq!(got["cookies"].as_array().unwrap().len(), 2);
        c.ok("Network.deleteCookies", json!({"name": "a", "url": url}), sid).await;
        let got = c.ok("Storage.getCookies", json!({"browserContextId": ctx}), None).await;
        assert_eq!(got["cookies"].as_array().unwrap().len(), 1);
        c.ok("Storage.clearCookies", json!({"browserContextId": ctx}), None).await;
        let got = c.ok("Network.getAllCookies", json!({}), sid).await;
        assert!(got["cookies"].as_array().unwrap().is_empty());
        // Cookie jars are per context.
        c.ok("Storage.setCookies", json!({"cookies": [{"name": "d", "value": "x", "domain": "127.0.0.1", "path": "/"}]}), None)
            .await;
        let got = c.ok("Storage.getCookies", json!({"browserContextId": ctx}), None).await;
        assert!(got["cookies"].as_array().unwrap().is_empty());
        c.events.clear();

        // Fetch: pause, fulfill, then pause and fail.
        c.ok("Fetch.enable", json!({"patterns": [{"urlPattern": "*", "requestStage": "Request"}]}), sid).await;
        c.ok("Page.navigate", json!({"url": "http://127.0.0.1:8765/a"}), sid).await;
        let paused = c.event("Fetch.requestPaused", sid).await;
        assert_eq!(paused["request"]["url"], "http://127.0.0.1:8765/a");
        assert_eq!(paused["resourceType"], "Document");
        // "<title>Fulfilled</title>" in base64.
        c.ok(
            "Fetch.fulfillRequest",
            json!({"requestId": paused["requestId"], "responseCode": 200, "body": "PHRpdGxlPkZ1bGZpbGxlZDwvdGl0bGU+"}),
            sid,
        )
        .await;
        let mut done = false;
        while !done {
            let ev = c.event("Page.lifecycleEvent", sid).await;
            done = ev["name"] == "load";
        }
        let r = c.ok("Runtime.evaluate", json!({"expression": "document.title"}), sid).await;
        assert_eq!(r["result"]["value"], "Fulfilled");
        let e = c.call("Fetch.continueRequest", json!({"requestId": paused["requestId"]}), sid).await.unwrap_err();
        assert_eq!(e["message"], "Invalid InterceptionId.");

        c.ok("Page.navigate", json!({"url": "http://127.0.0.1:8765/b"}), sid).await;
        let paused = c.event("Fetch.requestPaused", sid).await;
        c.ok("Fetch.failRequest", json!({"requestId": paused["requestId"], "errorReason": "Failed"}), sid).await;
        let failed = c.event("Network.loadingFailed", sid).await;
        assert_eq!(failed["errorText"], "net::ERR_FAILED");

        c.events.clear();
        c.ok("Page.navigate", json!({"url": "http://127.0.0.1:8765/c"}), sid).await;
        let paused = c.event("Fetch.requestPaused", sid).await;
        c.ok("Fetch.continueRequest", json!({"requestId": paused["requestId"]}), sid).await;
        let nav = c.event("Page.frameNavigated", sid).await;
        assert_eq!(nav["frame"]["url"], "http://127.0.0.1:8765/c");
        c.ok("Fetch.disable", json!({}), sid).await;
    })
    .unwrap();
}

#[test]
fn emulation_is_checked_against_the_persona() {
    block_on_local(async {
        let (ws, _) = start(host()).await;
        let (mut c, _, _, s) = new_page(&ws).await;
        let sid = Some(s.as_str());
        // Playwright's default 1280x720 context contradicts a 1920x1080 screen.
        let e = c
            .call(
                "Emulation.setDeviceMetricsOverride",
                json!({"mobile": false, "width": 1280, "height": 720, "screenWidth": 1280, "screenHeight": 720,
                       "deviceScaleFactor": 1, "screenOrientation": {"angle": 0, "type": "landscapePrimary"}}),
                sid,
            )
            .await
            .unwrap_err();
        assert_eq!(e["code"], -32000);
        assert!(e["message"].as_str().unwrap().contains("contradicts the persona"), "{e}");
        // A smaller window on the persona's screen is fine.
        c.ok(
            "Emulation.setDeviceMetricsOverride",
            json!({"mobile": false, "width": 1280, "height": 720, "screenWidth": 1920, "screenHeight": 1080, "deviceScaleFactor": 1}),
            sid,
        )
        .await;
        let m = c.ok("Page.getLayoutMetrics", json!({}), sid).await;
        assert_eq!(m["cssLayoutViewport"]["clientWidth"], 1280);
        c.ok("Emulation.clearDeviceMetricsOverride", json!({}), sid).await;
        let m = c.ok("Page.getLayoutMetrics", json!({}), sid).await;
        assert_eq!(m["cssLayoutViewport"]["clientWidth"], 1920);
        let e = c.call("Emulation.setDeviceMetricsOverride", json!({"mobile": true, "width": 390, "height": 844, "deviceScaleFactor": 3}), sid).await;
        assert!(e.is_err());

        let ff = "Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:128.0) Gecko/20100101 Firefox/128.0";
        let e = c.call("Emulation.setUserAgentOverride", json!({"userAgent": ff}), sid).await.unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("claims firefox"));
        let e = c.call("Network.setUserAgentOverride", json!({"userAgent": ff}), sid).await.unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("claims firefox"));
        c.ok("Emulation.setUserAgentOverride", json!({"userAgent": "", "acceptLanguage": "en-US"}), sid).await;
        let ua = persona(WIN).browser.user_agent;
        c.ok("Emulation.setUserAgentOverride", json!({"userAgent": ua}), sid).await;
        assert!(c.call("Emulation.setLocaleOverride", json!({"locale": "de-DE"}), sid).await.is_err());
        c.ok("Emulation.setLocaleOverride", json!({"locale": "en-US"}), sid).await;
        assert!(c.call("Emulation.setTimezoneOverride", json!({"timezoneId": "Asia/Tokyo"}), sid).await.is_err());
        c.ok("Emulation.setTimezoneOverride", json!({"timezoneId": "America/New_York"}), sid).await;
        let e = c
            .call("Emulation.setEmulatedMedia", json!({"features": [{"name": "prefers-color-scheme", "value": "dark"}]}), sid)
            .await;
        assert!(e.is_err());
        assert!(c.call("Emulation.setTouchEmulationEnabled", json!({"enabled": true}), sid).await.is_err());
        c.ok("Emulation.setTouchEmulationEnabled", json!({"enabled": false}), sid).await;
        assert!(c.call("Emulation.setGeolocationOverride", json!({"latitude": 1, "longitude": 2}), sid).await.is_err());
        c.ok("Emulation.setGeolocationOverride", json!({}), sid).await;
    })
    .unwrap();
}

#[test]
fn targets_sessions_and_personas() {
    block_on_local(async {
        let (ws, _) = start(host()).await;
        let (mut c, ctx, t, s) = new_page(&ws).await;

        // A second, explicitly attached session on the same page.
        let s2 = c
            .ok(
                "Target.attachToTarget",
                json!({"targetId": t, "flatten": true}),
                None,
            )
            .await["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        assert_ne!(s2, s);
        c.ok("Runtime.enable", json!({}), Some(&s2)).await;
        let ev = c.event("Runtime.executionContextCreated", Some(&s2)).await;
        assert_eq!(ev["context"]["auxData"]["isDefault"], true);
        // Events go only to sessions that enabled the domain.
        c.ok(
            "Page.navigate",
            json!({"url": "http://127.0.0.1:8765/"}),
            Some(&s),
        )
        .await;
        let _ = c.event("Page.frameNavigated", Some(&s)).await;
        c.ok("Runtime.evaluate", json!({"expression": "1"}), Some(&s2))
            .await;
        assert!(
            c.events
                .iter()
                .all(|m| !(m["method"] == "Page.frameNavigated" && m["sessionId"] == s2.as_str()))
        );
        c.ok("Target.detachFromTarget", json!({"sessionId": s2}), None)
            .await;
        let ev = c.event("Target.detachedFromTarget", None).await;
        assert_eq!(ev["sessionId"], s2.as_str());
        let e = c
            .call("Runtime.evaluate", json!({"expression": "1"}), Some(&s2))
            .await
            .unwrap_err();
        assert_eq!(e["code"], -32001);

        let infos = c.ok("Target.getTargets", json!({}), None).await;
        assert_eq!(infos["targetInfos"][0]["attached"], true);
        let b = c.ok("Target.getTargetInfo", json!({}), None).await;
        assert_eq!(b["targetInfo"]["type"], "browser");
        let p = c.ok("Target.getTargetInfo", json!({}), Some(&s)).await;
        assert_eq!(p["targetInfo"]["targetId"], t.as_str());
        let bs = c.ok("Target.attachToBrowserTarget", json!({}), None).await["sessionId"]
            .as_str()
            .unwrap()
            .to_string();
        let v = c.ok("Browser.getVersion", json!({}), Some(&bs)).await;
        assert_eq!(v["product"], "Chrome/153.0.8010.12");
        assert_eq!(v["protocolVersion"], "1.3");
        let ctxs = c.ok("Target.getBrowserContexts", json!({}), None).await;
        assert_eq!(ctxs["browserContextIds"], json!([ctx]));

        // setPersona: wrong family, live pages, then a fresh context.
        let mut ff: Value = serde_json::from_str(&persona_json(WIN)).unwrap();
        ff["browser"]["family"] = json!("firefox");
        let e = c
            .call(
                "LightBrowser.setPersona",
                json!({"persona": ff, "browserContextId": ctx}),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            e["message"]
                .as_str()
                .unwrap()
                .contains("LightBrowser.setPersona"),
            "{e}"
        );
        let mac: Value = serde_json::from_str(&persona_json(MAC)).unwrap();
        let e = c
            .call(
                "LightBrowser.setPersona",
                json!({"persona": mac, "browserContextId": ctx}),
                None,
            )
            .await
            .unwrap_err();
        assert!(
            e["message"]
                .as_str()
                .unwrap()
                .contains("close the context's pages")
        );
        let ctx2 = c.ok("Target.createBrowserContext", json!({}), None).await["browserContextId"]
            .as_str()
            .unwrap()
            .to_string();
        let r = c
            .ok(
                "LightBrowser.setPersona",
                json!({"persona": mac, "browserContextId": ctx2}),
                None,
            )
            .await;
        assert_eq!(r["browserContextId"], ctx2.as_str());
        c.events.clear();
        let t2 = c
            .ok(
                "Target.createTarget",
                json!({"url": "about:blank", "browserContextId": ctx2}),
                None,
            )
            .await;
        let a = c.event("Target.attachedToTarget", None).await;
        assert_eq!(a["targetInfo"]["targetId"], t2["targetId"]);
        let s3 = a["sessionId"].as_str().unwrap().to_string();
        let r = c
            .ok(
                "Runtime.evaluate",
                json!({"expression": "navigator.platform"}),
                Some(&s3),
            )
            .await;
        assert_eq!(r["result"]["value"], "MacIntel");
        // The Mac persona's DPR is 2: DPR 1 now contradicts it.
        let e = c
            .call(
                "Emulation.setDeviceMetricsOverride",
                json!({"mobile": false, "width": 0, "height": 0, "deviceScaleFactor": 1}),
                Some(&s3),
            )
            .await;
        assert!(e.is_err());
        // A browser session must name the page; the mock host has no report.
        let e = c
            .call("LightBrowser.fingerprintReport", json!({}), None)
            .await
            .unwrap_err();
        assert!(e["data"].as_str().unwrap().contains("targetId"), "{e}");
        let e = c
            .call("LightBrowser.fingerprintReport", json!({}), Some(&s3))
            .await
            .unwrap_err();
        assert!(e["message"].as_str().unwrap().contains("not supported"));
        for m in ["humanClick", "humanType", "humanScroll"] {
            let e = c
                .call(&format!("LightBrowser.{m}"), json!({}), None)
                .await
                .unwrap_err();
            assert!(
                e["message"]
                    .as_str()
                    .unwrap()
                    .contains("not implemented yet")
            );
        }

        // Closing and disposing.
        c.ok("Target.closeTarget", json!({"targetId": t}), None)
            .await;
        let ev = c.event("Target.detachedFromTarget", None).await;
        assert_eq!(ev["targetId"], t.as_str());
        c.ok(
            "Target.disposeBrowserContext",
            json!({"browserContextId": ctx2}),
            None,
        )
        .await;
        let ev = c.event("Target.detachedFromTarget", None).await;
        assert_eq!(ev["targetId"], t2["targetId"]);
        let infos = c.ok("Target.getTargets", json!({}), None).await;
        assert!(infos["targetInfos"].as_array().unwrap().is_empty());
        let e = c
            .call(
                "Target.disposeBrowserContext",
                json!({"browserContextId": "nope"}),
                None,
            )
            .await;
        assert!(e.is_err());
    })
    .unwrap();
}

#[test]
fn contexts_die_with_their_connection_and_browser_close_stops() {
    block_on_local(async {
        let h = host();
        let cfg = ServerConfig {
            addr: "127.0.0.1:0".parse().unwrap(),
            allow_origins: Vec::new(),
        };
        let server = Server::bind(cfg, h).await.unwrap();
        let url = server.browser_ws_url();
        let done = tokio::task::spawn_local(server.run());

        let (c, _, _, _) = new_page(&url).await;
        drop(c);
        tokio::time::sleep(Duration::from_millis(100)).await;
        let mut c2 = Client::connect(&url).await;
        let infos = c2.ok("Target.getTargets", json!({}), None).await;
        assert!(
            infos["targetInfos"].as_array().unwrap().is_empty(),
            "disposeOnDetach context survived"
        );

        // Pages in the default context outlive connections and auto-attach.
        let t = c2
            .ok("Target.createTarget", json!({"url": "about:blank"}), None)
            .await;
        drop(c2);
        let mut c3 = Client::connect(&url).await;
        c3.ok(
            "Target.setAutoAttach",
            json!({"autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true}),
            None,
        )
        .await;
        let a = c3.event("Target.attachedToTarget", None).await;
        assert_eq!(a["targetInfo"]["targetId"], t["targetId"]);
        c3.ok("Browser.close", json!({}), None).await;
        tokio::time::timeout(Duration::from_secs(5), done)
            .await
            .unwrap()
            .unwrap()
            .unwrap();
    })
    .unwrap();
}
