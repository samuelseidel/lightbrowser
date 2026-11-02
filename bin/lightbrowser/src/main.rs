//! `lightbrowser` CLI. Build per family:
//!   cargo build --release                                            # chrome
//!   cargo build --release --no-default-features --features firefox  # firefox

use std::process::ExitCode;

const USAGE: &str = "\
lightbrowser - headless browser for detection testing

USAGE:
  lightbrowser --version
  lightbrowser serve --port 9222 [--persona FILE]   (M3)
  lightbrowser fetch <url> [--dump html|text]       (M1)
";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            let family = lb_js::COMPILED_FAMILY.map_or("none", |f| f.as_str());
            println!("lightbrowser {} ({family})", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("serve" | "fetch") => {
            eprintln!("not implemented yet; see docs/ROADMAP.md");
            ExitCode::from(2)
        }
        _ => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
    }
}
