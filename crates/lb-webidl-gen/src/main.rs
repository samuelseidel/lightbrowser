//! `lb-webidl-gen --family chrome [--fixture F] [--idl DIR] [--out FILE] [--check]`

use std::path::PathBuf;
use std::process::ExitCode;

use lb_webidl_gen::{ChromeInputs, build_chrome, render_chrome};

fn main() -> ExitCode {
    let mut inputs = ChromeInputs::default();
    let mut family = String::from("chrome");
    let mut check = false;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut value = || args.next().map(PathBuf::from);
        match a.as_str() {
            "--family" => family = value().map(|p| p.display().to_string()).unwrap_or_default(),
            "--fixture" => inputs.fixture = value().unwrap_or(inputs.fixture),
            "--idl" => inputs.idl_root = value().unwrap_or(inputs.idl_root),
            "--out" => inputs.out = value().unwrap_or(inputs.out),
            "--check" => check = true,
            "-h" | "--help" => {
                println!(
                    "usage: lb-webidl-gen --family chrome [--fixture FILE] [--idl DIR] [--out FILE] [--check]"
                );
                return ExitCode::SUCCESS;
            }
            other => {
                eprintln!("unknown argument {other}");
                return ExitCode::from(2);
            }
        }
    }
    if family != "chrome" {
        eprintln!("only --family chrome exists for now (Gecko IDL arrives in M4b)");
        return ExitCode::from(2);
    }
    let table = match build_chrome(&inputs.idl_root, &inputs.fixture) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    let text = render_chrome(&table);
    eprintln!("coverage: {}", table.coverage.summary());
    if check {
        let current = std::fs::read_to_string(&inputs.out).unwrap_or_default();
        if current != text {
            eprintln!("{} is out of date", inputs.out.display());
            return ExitCode::FAILURE;
        }
        return ExitCode::SUCCESS;
    }
    if let Err(e) = std::fs::write(&inputs.out, text) {
        eprintln!("error: writing {}: {e}", inputs.out.display());
        return ExitCode::FAILURE;
    }
    eprintln!(
        "wrote {} ({} interfaces, {} window props)",
        inputs.out.display(),
        table.interfaces.len(),
        table.window_props.len()
    );
    ExitCode::SUCCESS
}
