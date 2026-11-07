//! `CSS.supports()` answers against Chrome 153's on the recorded corpus
//! (`lb-page/tests/css_supports/corpus.jsonl`, made by
//! `tools/css_supports/gen_cases.py`, answers recorded by
//! `tools/css_supports/record.py --corpus`). The same corpus runs through a
//! real page in lb-page's `css_supports` test; this one is the quick check.
//! `LB_SUPPORTS_CORPUS=path` runs another corpus file instead.

use lb_css::{supports_condition, supports_declaration};

#[test]
fn corpus_matches_chrome_153() {
    let path = std::env::var("LB_SUPPORTS_CORPUS").unwrap_or_else(|_| {
        format!(
            "{}/../lb-page/tests/css_supports/corpus.jsonl",
            env!("CARGO_MANIFEST_DIR")
        )
    });
    let text = std::fs::read_to_string(&path).unwrap();
    let (mut total, mut fails) = (0, Vec::new());
    for line in text.lines() {
        let Ok(serde_json::Value::Array(c)) = serde_json::from_str(line) else {
            continue;
        };
        let want = c.last().and_then(|v| v.as_bool()).expect("answer");
        let got = match c.len() {
            3 => supports_declaration(c[0].as_str().unwrap(), c[1].as_str().unwrap()),
            _ => supports_condition(c[0].as_str().unwrap()),
        };
        total += 1;
        if got != want {
            fails.push(format!(
                "{} chrome {want}",
                serde_json::to_string(&c[..c.len() - 1]).unwrap()
            ));
        }
    }
    if let Ok(out) = std::env::var("LB_SUPPORTS_REPORT") {
        std::fs::write(out, fails.join("\n")).unwrap();
    }
    eprintln!("{} of {total} match Chrome", total - fails.len());
    assert!(total > 1000);
    assert!(
        fails.is_empty(),
        "{} mismatches:\n{}",
        fails.len(),
        fails[..fails.len().min(50)].join("\n")
    );
}
