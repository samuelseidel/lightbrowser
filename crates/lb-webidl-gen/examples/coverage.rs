//! Print the coverage report: `cargo run -p lb-webidl-gen --example coverage`.
fn main() {
    let i = lb_webidl_gen::ChromeInputs::default();
    let t = lb_webidl_gen::build_chrome(&i).unwrap();
    let c = &t.coverage;
    println!("{}", c.summary());
    let verbose = std::env::args().any(|a| a == "-v");
    if verbose {
        for w in &c.warnings {
            println!("warning: {w}");
        }
        for m in &c.fixture_only {
            println!("fixture-only: {m}");
        }
        for m in &c.length_mismatch {
            println!("length: {m}");
        }
        for m in &c.order_mismatch {
            println!("order: {m}");
        }
    }
}
