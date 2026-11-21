//! Writes generated personas: consistent combinations sampled from
//! `profiles/catalog/` (see `lb_persona::generate`). Refuses (exit 1) if a
//! persona breaks a consistency rule.
//!
//! ```text
//! cargo run -p lb-persona --example gen_personas -- --os linux --count 5 --seed 1 \
//!     --out profiles/generated/linux [--catalog profiles/catalog]
//! ```

use std::path::PathBuf;

use lb_persona::OsKind;
use lb_persona::generate::{Catalog, generate};

fn main() {
    if let Err(e) = run() {
        eprintln!("gen_personas: {e}");
        std::process::exit(1);
    }
}

fn run() -> Result<(), String> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../profiles");
    let mut os = None;
    let mut count = 5usize;
    let mut seed = 1u64;
    let mut out = None;
    let mut catalog = root.join("catalog");
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        let mut val = || args.next().ok_or(format!("{a} needs a value"));
        match a.as_str() {
            "--os" => os = Some(OsKind::from_name(&val()?)),
            "--count" => count = val()?.parse().map_err(|e| format!("--count: {e}"))?,
            "--seed" => seed = val()?.parse().map_err(|e| format!("--seed: {e}"))?,
            "--out" => out = Some(PathBuf::from(val()?)),
            "--catalog" => catalog = PathBuf::from(val()?),
            "-h" | "--help" => {
                println!(
                    "gen_personas --os windows|macos|linux [--count N] [--seed S] [--out DIR] [--catalog DIR]"
                );
                return Ok(());
            }
            other => return Err(format!("unknown argument {other}")),
        }
    }
    let os = os.ok_or("--os windows|macos|linux is required")?;
    if os == OsKind::Other {
        return Err("--os must be windows, macos or linux".into());
    }
    let out = out.unwrap_or_else(|| root.join("generated").join(os.as_str()));
    let cat = Catalog::load(&catalog)?;
    let personas = generate(&cat, os, count, seed)?;
    std::fs::create_dir_all(&out).map_err(|e| format!("{}: {e}", out.display()))?;
    for (i, p) in personas.iter().enumerate() {
        let path = out.join(format!("chrome153_{}_s{seed}_{i:03}.json", os.as_str()));
        let mut text = serde_json::to_string_pretty(p).map_err(|e| e.to_string())?;
        text.push('\n');
        std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
        println!("{}  {}", path.display(), p.profile_name);
    }
    Ok(())
}
