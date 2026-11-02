#[test]
fn smoke() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("idl/chromium");
    let list = std::fs::read_to_string(root.join("FILES.txt")).unwrap();
    let mut errs = 0;
    for rel in list.lines().filter(|l| !l.is_empty()) {
        let src = std::fs::read_to_string(root.join(rel)).unwrap();
        if let Err(e) = lb_webidl_gen::parse::parse_idl(&src, rel) { println!("{e}"); errs += 1; }
    }
    assert_eq!(errs, 0);
}
