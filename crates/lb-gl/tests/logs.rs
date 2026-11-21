//! Shader compile results and info logs vs Chrome for Testing 153
//! (`tests/chrome/logs.json`, recorded with `tests/chrome/record.mjs`).

use lb_gl::glsl::{Options, Stage, compile};

/// Deeply nested shaders fail to compile instead of overflowing the stack.
/// Logs recorded in Chrome for Testing 153 (loopback, SwiftShader):
/// ANGLE's parser stack ends at 10000 ("memory exhausted"); a tree deeper
/// than 255 levels is "Expression too complex.".
#[test]
fn deep_nesting_is_a_compile_error() {
    let opts = Options {
        webgl2: false,
        extensions: Vec::new(),
        max_draw_buffers: 8,
    };
    let parens = |n: usize| {
        format!(
            "void main(){{ gl_Position = vec4({}1.0{}); }}",
            "(".repeat(n),
            ")".repeat(n)
        )
    };
    let chain = |n: usize| {
        format!(
            "attribute float a; void main(){{ gl_Position = vec4(a{}); }}",
            "+a".repeat(n)
        )
    };
    let blocks = |n: usize| {
        format!(
            "void main(){{ {}gl_Position = vec4(1.0);{} }}",
            "{".repeat(n),
            "}".repeat(n)
        )
    };
    let calls = |n: usize| {
        format!(
            "attribute float a; void main(){{ gl_Position = vec4({}a{}); }}",
            "sin(".repeat(n),
            ")".repeat(n)
        )
    };
    let ifs = |n: usize| {
        format!(
            "attribute float a; void main(){{ {}gl_Position = vec4(1.0); }}",
            "if (a > 0.0) ".repeat(n)
        )
    };
    let exhausted = |t: &str| format!("ERROR: 0:1: '{t}' : memory exhausted\n");
    let complex = "ERROR: Expression too complex.\n".to_string();
    let cases = [
        (parens(100_000), Some(exhausted("("))),
        (parens(10_000), Some(exhausted("("))),
        (parens(1000), None),
        (chain(100_000), Some(complex.clone())),
        (chain(300), Some(complex.clone())),
        (chain(100), None),
        (blocks(100_000), Some(exhausted("{"))),
        (blocks(300), Some(complex.clone())),
        (blocks(100), None),
        (calls(100_000), Some(exhausted("("))),
        (calls(300), Some(complex.clone())),
        (calls(100), None),
        // Chrome names the `0.0` where its stack ran out; any token here.
        (ifs(100_000), Some(String::new())),
        (ifs(300), Some(complex.clone())),
        (ifs(100), None),
    ];
    for (src, want) in cases {
        let (sh, log) = compile(&src, Stage::Vertex, &opts);
        let head = &src[..src.len().min(60)];
        match want {
            Some(w) if w.is_empty() => {
                assert!(sh.is_none(), "{head}");
                assert!(log.starts_with("ERROR: 0:1: '"), "{head}: {log}");
                assert!(log.ends_with("' : memory exhausted\n"), "{head}: {log}");
            }
            Some(w) => {
                assert!(sh.is_none(), "{head}");
                assert_eq!(log, w, "{head}");
            }
            None => assert!(sh.is_some(), "{head}: {log}"),
        }
    }
}

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

/// Long postfix chains, `#if` expressions and call chains are refused or
/// bounded instead of overflowing the stack or running for ever. The inputs
/// are built here in code; nothing about them was recorded from Chrome, so
/// the test checks that the compile fails (and, for the limits that have
/// ANGLE's text, which error) and that it finishes.
#[test]
fn long_chains_are_compile_errors() {
    let opts = Options {
        webgl2: false,
        extensions: Vec::new(),
        max_draw_buffers: 8,
    };
    let vs = |body: String| format!("void main(){{ {body} }}");
    let main_only = "void main(){ gl_Position = vec4(1.0); }";
    // `f.x.x.x...`, `v[0][0]...` and `f++++...`: each link wraps the tree once more.
    let swizzles = |n: usize| {
        vs(format!(
            "float f = 1.0; gl_Position = vec4(f{});",
            ".x".repeat(n)
        ))
    };
    let indices = |n: usize| {
        vs(format!(
            "vec4 v = vec4(1.0); gl_Position = vec4(v{});",
            "[0]".repeat(n)
        ))
    };
    let incs = |n: usize| {
        vs(format!(
            "float f = 1.0; gl_Position = vec4(f{});",
            "++".repeat(n)
        ))
    };
    // `#if` lines: parentheses, prefix operators and `?:` chains recurse once per level.
    let if_parens = |n: usize| {
        format!(
            "#if {}1{}\n#endif\n{main_only}",
            "(".repeat(n),
            ")".repeat(n)
        )
    };
    let if_nots = |n: usize| format!("#if {}1\n#endif\n{main_only}", "!".repeat(n));
    let if_ternary = |n: usize| format!("#if {}1\n#endif\n{main_only}", "1?1:".repeat(n));
    // f0 is the leaf; f{i} calls f{i-1}; main calls the last one.
    let call_chain = |n: usize| {
        let mut s = String::from("float f0(){ return 1.0; }\n");
        for i in 1..=n {
            s += &format!("float f{i}(){{ return f{}(); }}\n", i - 1);
        }
        s + &format!("void main(){{ gl_Position = vec4(f{n}()); }}")
    };
    // g{i} calls g{i-1} twice: inlined naively that is 2^n calls.
    let call_tree = |n: usize| {
        let mut s = String::from("float g0(){ return 1.0; }\n");
        for i in 1..=n {
            s += &format!("float g{i}(){{ return g{}() + g{}(); }}\n", i - 1, i - 1);
        }
        s + &format!("void main(){{ gl_Position = vec4(g{n}()); }}")
    };

    let complex = "Expression too complex";
    let if_complex = "conditional expression too complex";
    let too_deep = "Call stack too deep (larger than 256)";
    let cases: Vec<(String, String, Option<&str>)> = vec![
        ("swizzle chain".into(), swizzles(200_000), Some(complex)),
        ("index chain".into(), indices(200_000), None),
        ("postfix ++ chain".into(), incs(200_000), None),
        (
            "#if parentheses".into(),
            if_parens(100_000),
            Some(if_complex),
        ),
        ("#if prefix !".into(), if_nots(300_000), Some(if_complex)),
        ("#if ?: chain".into(), if_ternary(100_000), Some(if_complex)),
        ("call chain 1000".into(), call_chain(1000), Some(too_deep)),
        ("call tree 40".into(), call_tree(40), Some(complex)),
    ];
    for (name, src, want) in cases {
        // A hung compile must fail the test, not hang it.
        let (tx, rx) = std::sync::mpsc::channel();
        let o = opts.clone();
        std::thread::spawn(move || {
            let (sh, log) = compile(&src, Stage::Vertex, &o);
            let _ = tx.send((sh.is_some(), log));
        });
        let (ok, log) = rx
            .recv_timeout(std::time::Duration::from_secs(60))
            .unwrap_or_else(|_| panic!("{name}: the compile did not finish"));
        assert!(!ok, "{name}: compiled");
        if let Some(w) = want {
            assert!(
                log.contains(w),
                "{name}: log is {:?}",
                &log[..log.len().min(300)]
            );
        }
    }
    // Chains and calls within the limits still compile.
    for (name, src) in [
        ("swizzle chain 100", swizzles(100)),
        ("call chain 200", call_chain(200)),
        ("call tree 12", call_tree(12)),
        ("#if depth 50", if_parens(50)),
    ] {
        let (sh, log) = compile(&src, Stage::Vertex, &opts);
        assert!(sh.is_some(), "{name}: {log}");
    }
}
