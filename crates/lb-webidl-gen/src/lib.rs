pub mod parse;

use std::path::Path;

/// Parse every file listed in `<root>/FILES.txt`, in that order.
pub fn parse_dir(root: &Path) -> Result<Vec<parse::IdlFile>, Box<dyn std::error::Error>> {
    let list = std::fs::read_to_string(root.join("FILES.txt"))?;
    let mut out = Vec::new();
    for rel in list.lines().map(str::trim).filter(|l| !l.is_empty()) {
        let src = std::fs::read_to_string(root.join(rel))?;
        out.push(parse::parse_idl(&src, rel)?);
    }
    Ok(out)
}
