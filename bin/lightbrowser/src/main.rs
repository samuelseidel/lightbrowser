//! `lightbrowser` CLI. Build per family:
//!   cargo build --release                                            # chrome
//!   cargo build --release --no-default-features --features firefox  # firefox

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lb_net::fetch::{NetContext, Request};
use lb_net::{ClientHelloProfile, HttpVersion};
use lb_persona::Persona;

const USAGE: &str = "\
lightbrowser - headless browser for detection testing

USAGE:
  lightbrowser --version
  lightbrowser serve --port 9222 [--persona FILE]   (M3)
  lightbrowser fetch <url> [OPTIONS]

FETCH OPTIONS:
  --dump html|text       print the body (text = tags stripped); default: none
  --persona NAME         chrome | firefox (default: the compiled family)
                         PATH.json personas are not supported yet (M4)
  --profiles-dir DIR     TLS profiles (default: nearest profiles/tls above
                         the current directory or the executable)
  --insecure             accept invalid certificates (local test servers only)
  -v, --verbose          print the status line and headers to stderr

Exit code: 0 on 2xx/3xx, 1 on other statuses or errors, 2 on bad usage.
";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dump {
    None,
    Html,
    Text,
}

#[derive(Debug)]
struct FetchArgs {
    url: String,
    dump: Dump,
    persona: Option<String>,
    profiles_dir: Option<PathBuf>,
    insecure: bool,
    verbose: bool,
}

fn parse_fetch_args(args: &[String]) -> Result<FetchArgs, String> {
    let mut url = None;
    let mut out = FetchArgs {
        url: String::new(),
        dump: Dump::None,
        persona: None,
        profiles_dir: None,
        insecure: false,
        verbose: false,
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        // Accept both `--flag value` and `--flag=value`.
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_owned())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--dump" => {
                out.dump = match value("--dump")?.as_str() {
                    "html" => Dump::Html,
                    "text" => Dump::Text,
                    other => return Err(format!("--dump must be html or text, got {other:?}")),
                }
            }
            "--persona" => out.persona = Some(value("--persona")?),
            "--profiles-dir" => out.profiles_dir = Some(PathBuf::from(value("--profiles-dir")?)),
            "--insecure" => out.insecure = true,
            "-v" | "--verbose" => out.verbose = true,
            f if f.starts_with('-') => return Err(format!("unknown option: {f}")),
            _ => {
                if url.replace(arg.clone()).is_some() {
                    return Err("only one url is accepted".into());
                }
            }
        }
    }
    out.url = url.ok_or("fetch needs a url")?;
    Ok(out)
}

/// Pick the persona. Named personas must match the compiled engine family.
fn select_persona(name: Option<&str>) -> Result<Persona, String> {
    let compiled = lb_js::COMPILED_FAMILY;
    let persona = match name {
        Some("chrome") => Persona::default_chrome(),
        Some("firefox") => Persona::default_firefox(),
        Some(path) => {
            return Err(format!(
                "persona files are not supported yet (M4): {path:?}; use chrome or firefox"
            ));
        }
        None => match compiled {
            Some(lb_persona::BrowserFamily::Firefox) => Persona::default_firefox(),
            _ => Persona::default_chrome(),
        },
    };
    match compiled {
        Some(f) if f == persona.family => Ok(persona),
        Some(f) => Err(format!(
            "persona family {} does not match this build ({f}); rebuild with {}",
            persona.family,
            match persona.family {
                lb_persona::BrowserFamily::Chrome => "the default features",
                lb_persona::BrowserFamily::Firefox => "--no-default-features --features firefox",
            }
        )),
        None => Err("this build has no JS engine family enabled".into()),
    }
}

/// Nearest `profiles/tls` directory at or above `start`.
fn find_profiles_upward(start: &Path) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join("profiles").join("tls"))
        .find(|d| d.is_dir())
}

fn find_profiles_dir() -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|d| find_profiles_upward(&d))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            find_profiles_upward(exe.parent()?)
        })
}

fn run_fetch(args: &FetchArgs) -> Result<i32, String> {
    let persona = Arc::new(select_persona(args.persona.as_deref())?);
    let url = url::Url::parse(&args.url).map_err(|e| format!("invalid url {:?}: {e}", args.url))?;

    let dir = match &args.profiles_dir {
        Some(d) => d.clone(),
        None => find_profiles_dir().ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    let profile_path = dir.join(format!("{}.json", persona.tls_profile_id));
    let profile = ClientHelloProfile::load(&profile_path)
        .map_err(|e| format!("loading {}: {e}", profile_path.display()))?;

    let mut ctx = NetContext::new(persona, Arc::new(profile)).map_err(|e| e.to_string())?;
    ctx.danger_accept_invalid_certs = args.insecure;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting runtime: {e}"))?;
    let resp = rt
        .block_on(lb_net::fetch(&mut ctx, Request::get(url)))
        .map_err(|e| e.to_string())?;

    let version = match resp.version {
        HttpVersion::Http1 => "HTTP/1.1",
        HttpVersion::Http2 => "HTTP/2",
    };
    if args.verbose {
        eprintln!("{version} {}", resp.status);
        for (n, v) in &resp.headers {
            eprintln!("{n}: {v}");
        }
        eprintln!();
    }
    eprintln!(
        "{} {} {} ({} bytes)",
        version,
        resp.status.as_u16(),
        resp.final_url,
        resp.body.len()
    );

    let mut stdout = std::io::stdout().lock();
    let written = match args.dump {
        Dump::None => Ok(()),
        Dump::Html => stdout.write_all(&resp.body),
        Dump::Text => {
            let text = strip_tags(&String::from_utf8_lossy(&resp.body));
            stdout.write_all(text.as_bytes())
        }
    };
    // A closed pipe (`| head`) is not an error for us.
    if let Err(e) = written.and_then(|()| stdout.flush())
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(format!("writing output: {e}"));
    }

    let code = resp.status.as_u16();
    Ok(if (200..400).contains(&code) { 0 } else { 1 })
}

/// Tags that start a new line in text output.
const BLOCK_TAGS: &[&str] = &[
    "br", "p", "div", "li", "ul", "ol", "tr", "table", "title", "pre", "section", "article",
    "header", "footer", "h1", "h2", "h3", "h4", "h5", "h6",
];

/// Quick tag strip until the DOM exists (M2): drops tags, comments and the
/// contents of `script`/`style`, decodes `&amp; &lt; &gt; &quot; &#39;`, and
/// puts block tags on their own lines. Not a parser.
fn strip_tags(html: &str) -> String {
    let bytes = html.as_bytes();
    let lower = html.to_ascii_lowercase();
    let mut out = String::with_capacity(html.len() / 2);
    let mut text_start = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'<' {
            i += 1;
            continue;
        }
        let rest = &html[i..];
        let is_tag = rest.starts_with("<!--")
            || bytes
                .get(i + 1)
                .is_some_and(|c| c.is_ascii_alphabetic() || matches!(c, b'/' | b'!' | b'?'));
        if !is_tag {
            i += 1; // a literal '<' in text
            continue;
        }
        out.push_str(&decode_entities(&html[text_start..i]));

        if rest.starts_with("<!--") {
            i = lower[i + 4..]
                .find("-->")
                .map_or(bytes.len(), |p| i + 4 + p + 3);
        } else {
            let end = tag_end(bytes, i);
            let inner = &lower[i + 1..end.saturating_sub(1).max(i + 1)];
            let closing = inner.starts_with('/');
            let name: String = inner
                .trim_start_matches('/')
                .chars()
                .take_while(|c| c.is_ascii_alphanumeric())
                .collect();
            i = end;
            if BLOCK_TAGS.contains(&name.as_str()) && !out.is_empty() && !out.ends_with('\n') {
                out.push('\n');
            }
            if !closing && matches!(name.as_str(), "script" | "style") && !inner.ends_with('/') {
                let close = format!("</{name}");
                i = match lower[i..].find(&close) {
                    Some(p) => tag_end(bytes, i + p),
                    None => bytes.len(),
                };
            }
        }
        text_start = i;
    }
    if text_start < bytes.len() {
        out.push_str(&decode_entities(&html[text_start..]));
    }
    out
}

/// Index just past the `>` that closes the tag starting at `start`. A quote
/// only opens a quoted value right after `=`, so a stray apostrophe in an
/// attribute does not swallow the page.
fn tag_end(bytes: &[u8], start: usize) -> usize {
    let mut quote = None;
    let mut prev = 0u8; // last non-whitespace byte outside quotes
    for (off, &c) in bytes[start + 1..].iter().enumerate() {
        match quote {
            Some(q) => {
                if c == q {
                    quote = None;
                    prev = c;
                }
            }
            None => match c {
                b'>' => return start + 1 + off + 1,
                b'"' | b'\'' if prev == b'=' => quote = Some(c),
                c if c.is_ascii_whitespace() => {}
                c => prev = c,
            },
        }
    }
    bytes.len()
}

/// Decode the five basic entities in one pass (so `&amp;lt;` becomes `&lt;`).
fn decode_entities(text: &str) -> String {
    const ENTITIES: [(&str, char); 5] = [
        ("&amp;", '&'),
        ("&lt;", '<'),
        ("&gt;", '>'),
        ("&quot;", '"'),
        ("&#39;", '\''),
    ];
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(p) = rest.find('&') {
        out.push_str(&rest[..p]);
        rest = &rest[p..];
        match ENTITIES.iter().find(|(e, _)| rest.starts_with(e)) {
            Some((e, c)) => {
                out.push(*c);
                rest = &rest[e.len()..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            let family = lb_js::COMPILED_FAMILY.map_or("none", |f| f.as_str());
            println!("lightbrowser {} ({family})", env!("CARGO_PKG_VERSION"));
            ExitCode::SUCCESS
        }
        Some("fetch") => {
            if args[1..].iter().any(|a| a == "--help" || a == "-h") {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            let parsed = match parse_fetch_args(&args[1..]) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}\n");
                    eprint!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match run_fetch(&parsed) {
                Ok(code) => ExitCode::from(code as u8),
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
        }
        Some("serve") => {
            eprintln!("not implemented yet; see docs/ROADMAP.md");
            ExitCode::from(2)
        }
        None | Some("--help" | "-h" | "help") => {
            print!("{USAGE}");
            ExitCode::SUCCESS
        }
        Some(other) => {
            eprintln!("unknown argument: {other}\n");
            eprint!("{USAGE}");
            ExitCode::from(2)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_tags_scripts_styles_and_comments() {
        let html = "<html><head><title>T</title><style>p{color:red}</style>\
<script>if (a < b) { x('</p>'); }</script></head>\
<body><!-- hidden --><p class=\"a\">Hello <b>world</b></p><p>Next</p></body></html>";
        assert_eq!(strip_tags(html), "T\nHello world\nNext\n");
    }

    #[test]
    fn decodes_entities_once() {
        assert_eq!(
            strip_tags("<p>a &amp; b &lt;c&gt; &quot;d&quot; &#39;e&#39; &amp;lt; &nbsp;</p>"),
            "a & b <c> \"d\" 'e' &lt; &nbsp;\n"
        );
    }

    #[test]
    fn handles_odd_attributes_and_literal_angle_brackets() {
        assert_eq!(
            strip_tags("1 < 2 <a href=\"x>y\" title='z'>link</a> <img alt=it's> done"),
            "1 < 2 link  done"
        );
        // Unterminated tag and script drop the rest instead of panicking.
        assert_eq!(strip_tags("ok <div class=\"x"), "ok \n");
        assert_eq!(strip_tags("ok<script>never closed"), "ok");
    }

    #[test]
    fn parses_fetch_args() {
        let a = |v: &[&str]| parse_fetch_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        let p = a(&[
            "http://x/",
            "--dump=text",
            "--insecure",
            "-v",
            "--persona",
            "firefox",
        ])
        .unwrap();
        assert_eq!(p.dump, Dump::Text);
        assert!(p.insecure && p.verbose);
        assert_eq!(p.persona.as_deref(), Some("firefox"));
        assert!(a(&[]).is_err());
        assert!(a(&["u", "v"]).is_err());
        assert!(a(&["u", "--dump", "pdf"]).is_err());
        assert!(a(&["u", "--bogus"]).is_err());
    }

    #[test]
    fn persona_files_and_wrong_family_are_refused() {
        assert!(select_persona(Some("p.json")).unwrap_err().contains("M4"));
        let other = match lb_js::COMPILED_FAMILY {
            Some(lb_persona::BrowserFamily::Chrome) => "firefox",
            _ => "chrome",
        };
        assert!(
            select_persona(Some(other))
                .unwrap_err()
                .contains("does not match")
        );
        assert!(select_persona(None).is_ok());
    }
}
