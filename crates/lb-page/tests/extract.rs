//! `lb_page::extract` (ADR 0052): Markdown, links and JSON of loaded pages
//! against golden files in `tests/extract/`.
//!
//! The expectations are NOT browser recordings. They were written by hand
//! from the rules in the ADR and the module docs (the JSON files come from
//! `tests/extract/gen_json.py`, with hand-written metadata and text), so a
//! failure means the code or the rules changed, not that Chrome differs.
//! Each case is `NAME.html` (or `NAME.txt`, served as `text/plain`) with
//! `NAME.md`, `NAME.links` and `NAME.json`; `{HOST}` stands for the
//! loopback server's `127.0.0.1:PORT`. Pages are loaded for real (scripts
//! run, style sheets apply), so hidden-by-class content is covered too.
#![cfg(feature = "chrome")]

mod common;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use common::{Resource, browser, context, open, serve_fn};
use lb_page::WaitUntil;
use lb_page::extract::{json_dump, links_dump};

fn dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/extract")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(dir().join(name)).unwrap_or_else(|e| panic!("{name}: {e}"))
}

/// Case names: every `NAME.md`.
fn case_names() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir())
        .unwrap()
        .filter_map(|e| {
            let f = e.unwrap().file_name().into_string().unwrap();
            f.strip_suffix(".md").map(str::to_owned)
        })
        .collect();
    names.sort();
    names
}

/// The input file of a case and its content type.
fn input_of(name: &str) -> (String, &'static str) {
    if dir().join(format!("{name}.txt")).exists() {
        (format!("{name}.txt"), "text/plain; charset=utf-8")
    } else {
        (format!("{name}.html"), "text/html; charset=utf-8")
    }
}

fn show(what: &str, case: &str, want: &str, got: &str) -> String {
    format!("--- {case} {what}: expected\n{want}\n--- {case} {what}: got\n{got}\n---")
}

#[tokio::test]
async fn golden_markdown_links_and_json() {
    let names = case_names();
    assert!(names.len() >= 15, "only {} cases", names.len());
    let mut files: HashMap<String, (&'static str, Vec<u8>)> = HashMap::new();
    for n in &names {
        let (file, ct) = input_of(n);
        let body = std::fs::read(dir().join(&file)).unwrap();
        files.insert(format!("/{file}"), (ct, body));
    }
    let srv = serve_fn(Arc::new(move |path: &str| {
        files
            .get(path)
            .map(|(ct, body)| Resource::new(ct, body.clone()))
    }))
    .await;
    let host = format!("127.0.0.1:{}", srv.port);
    let b = browser();
    let ctx = context(false);
    let mut failures = Vec::new();
    for n in &names {
        let (file, _) = input_of(n);
        let url = srv.url(&format!("/{file}"));
        let page = open(&b, &ctx, &url, WaitUntil::Load).await;
        let e = page.extract().await.unwrap();
        let final_url = page.url().await.unwrap();
        let want_md = read(&format!("{n}.md")).replace("{HOST}", &host);
        if e.markdown != want_md {
            failures.push(show("markdown", n, &want_md, &e.markdown));
        }
        let want_links = read(&format!("{n}.links")).replace("{HOST}", &host);
        let got_links = links_dump(&e);
        if got_links != want_links {
            failures.push(show("links", n, &want_links, &got_links));
        }
        let want_json = read(&format!("{n}.json")).replace("{HOST}", &host);
        let got_json = json_dump(&e, &url, &final_url, 200);
        if got_json != want_json {
            failures.push(show("json", n, &want_json, &got_json));
        }
        page.close().await.unwrap();
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The text of the JSON is the `--dump text` string of the same page.
#[tokio::test]
async fn json_text_is_the_text_dump() {
    let srv = serve_fn(Arc::new(|path: &str| {
        (path == "/p").then(|| Resource::html("<title>T</title><h1>H</h1><p>a<br>b</p>"))
    }))
    .await;
    let b = browser();
    let ctx = context(false);
    let page = open(&b, &ctx, &srv.url("/p"), WaitUntil::Load).await;
    let e = page.extract().await.unwrap();
    assert_eq!(e.text, page.text().await.unwrap());
    assert_eq!(e.markdown, "# H\n\na\\\nb\n");
}

/// Hidden content follows the page's style, not only attributes.
#[tokio::test]
async fn style_sheet_and_script_changes_decide_what_is_hidden() {
    let srv = serve_fn(Arc::new(|path: &str| {
        (path == "/p").then(|| {
            Resource::html(
                "<style>.x{display:none}</style><body><p id=a>one</p><p id=b class=x>two</p>\
                 <script>document.getElementById('b').className='';\
                 document.getElementById('a').className='x'</script>",
            )
        })
    }))
    .await;
    let b = browser();
    let ctx = context(false);
    let page = open(&b, &ctx, &srv.url("/p"), WaitUntil::Load).await;
    let e = page.extract().await.unwrap();
    assert_eq!(e.markdown, "two\n");
}
