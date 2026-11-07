//! Console calls run exactly the page code Chrome runs for them: each case
//! in `cases/` makes console calls with arguments whose getters, `toString`,
//! `Symbol.toStringTag`, proxy traps and format conversions record a hit,
//! and the hit list (and which calls throw into the page) must equal what
//! Chrome 153 (Chrome for Testing 153.0.8010.12, `chrome-headless-shell`)
//! gave with no DevTools client attached, in `chrome.json`. Plain getters,
//! `toJSON`, an object's own `toString` and proxy traps must never run: they
//! would tell a page it is not in Chrome. Re-record with
//! `CHROME=/path/to/chrome-headless-shell sh record.sh > chrome.json`.
//! Included from `pages.rs`.

use std::path::PathBuf;
use std::sync::Arc;

use lb_page::WaitUntil;

use super::common::{Resource, browser, context, open, serve_fn};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/console")
}

fn route(path: &str) -> Option<Resource> {
    let case = path.trim_start_matches('/').trim_end_matches('/');
    let html = std::fs::read_to_string(dir().join("cases").join(format!("{case}.html"))).ok()?;
    Some(Resource::html(&html))
}

async fn run(case: &str) {
    let srv = serve_fn(Arc::new(route)).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url(&format!("/{case}")), WaitUntil::Load).await;
    let got = match page.evaluate("window.HITS").await.unwrap() {
        lb_js::JsValue::String(s) => s,
        v => panic!("{v:?}"),
    };
    let got: Vec<String> = serde_json::from_str(&got).unwrap();
    let all: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(dir().join("chrome.json")).unwrap()).unwrap();
    let want: Vec<String> = serde_json::from_value(all[case].clone()).unwrap();
    assert_eq!(got, want, "{case}");
}

macro_rules! cases {
    ($($name:ident),* $(,)?) => {
        $(
            #[tokio::test]
            async fn $name() {
                run(stringify!($name)).await;
            }
        )*
    };
}

cases!(arguments, conversions, labels_and_throws);

/// The text we report for a console call reads data properties only, so
/// plain objects keep their JSON while getters stay untouched.
#[tokio::test]
async fn message_text_reads_data_only() {
    let html = r#"<!doctype html><script>
        window.ran = [];
        console.log('a', 1, {x: 2, get y() { ran.push('y'); return 3; }, f() {}, n: [1, null, 'z']});
        console.log('%s and %d', 'str', '42px', {k: 1});
        console.log(new Proxy({}, {get() { ran.push('proxy'); }}));
        console.log([1, [2, 3]]);
        console.log(Symbol('q'), 10n, null, undefined);
        console.log();
    </script>"#;
    let srv = serve_fn(Arc::new(move |_: &str| Some(Resource::html(html)))).await;
    let (b, ctx) = (browser(), context(false));
    let page = open(&b, &ctx, &srv.url("/"), WaitUntil::Load).await;
    assert_eq!(
        page.evaluate("ran.join()").await.unwrap(),
        lb_js::JsValue::String(String::new())
    );
    let texts: Vec<String> = page
        .console_messages()
        .await
        .unwrap()
        .into_iter()
        .map(|m| m.text)
        .collect();
    assert_eq!(
        texts,
        [
            r#"a 1 {"x":2,"n":[1,null,"z"]}"#,
            r#"str and 42 {"k":1}"#,
            "[object Proxy]",
            "[1,[2,3]]",
            "Symbol(q) 10n null undefined",
        ],
    );
}
