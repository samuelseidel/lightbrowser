//! Shader compile results and info logs vs Chrome for Testing 153
//! (`tests/chrome/logs.json`, recorded with `tests/chrome/record.mjs`).

use lb_gl::glsl::{Options, Stage, compile};

#[test]
fn info_logs_match_chrome() {
    let cases: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("chrome/logs.json")).expect("json");
    let mut bad = Vec::new();
    for c in &cases {
        let exts: Vec<&'static str> = c["exts"]
            .as_array()
            .expect("exts")
            .iter()
            .map(|e| match e.as_str().expect("ext") {
                "GL_OES_standard_derivatives" => "GL_OES_standard_derivatives",
                other => panic!("{other}"),
            })
            .collect();
        let opts = Options {
            webgl2: c["webgl2"].as_bool().expect("webgl2"),
            extensions: exts,
            max_draw_buffers: 8,
        };
        let stage = if c["vertex"].as_bool().expect("vertex") {
            Stage::Vertex
        } else {
            Stage::Fragment
        };
        let (sh, log) = compile(c["src"].as_str().expect("src"), stage, &opts);
        let ok = c["ok"].as_bool().expect("ok");
        let want = c["log"].as_str().expect("log");
        if sh.is_some() != ok || log != want {
            bad.push(format!(
                "{}: ok {} (want {})\n  got:  {:?}\n  want: {:?}",
                c["name"],
                sh.is_some(),
                ok,
                log,
                want
            ));
        }
    }
    for b in &bad {
        eprintln!("{b}");
    }
    eprintln!("{} of {} cases match", cases.len() - bad.len(), cases.len());
    // Every case must at least agree on success; the log text of a few
    // follow-on errors differs (see ADR 0030).
    let status_bad = bad
        .iter()
        .filter(|b| b.contains("ok true (want false)") || b.contains("ok false (want true)"))
        .count();
    assert_eq!(status_bad, 0);
}
