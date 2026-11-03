//! `lightbrowser` CLI. Build per family:
//!   cargo build --release                                            # chrome
//!   cargo build --release --no-default-features --features firefox  # firefox

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lb_net::ClientHelloProfile;
use lb_persona::Persona;

const USAGE: &str = "\
lightbrowser - headless browser for detection testing

USAGE:
  lightbrowser --version
  lightbrowser serve [--port 9222] [--persona FILE] [--remote-allow-origins O]
                                            CDP server on 127.0.0.1 for Playwright
                                            connectOverCDP (mock page host until
                                            lb-page implements lb_cdp::PageHost)
  lightbrowser fetch <url> [OPTIONS]
  lightbrowser validate <persona.json>...   check v1/v2 personas against the
                                            consistency rules (exit 1 on any
                                            violation or load error)

FETCH OPTIONS:
  --dump html|text       print the DOM after scripts ran (html) or its text;
                         default: none
  --wait-until W         load (default) | networkidle | domcontentloaded |
                         commit | <ms> (load, then that many ms more)
  --timeout SECS         give up waiting after SECS (default 30)
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
    wait_until: String,
    timeout: u64,
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
        wait_until: "load".into(),
        timeout: 30,
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
            "--wait-until" => {
                let v = value("--wait-until")?;
                if !matches!(
                    v.as_str(),
                    "load" | "networkidle" | "domcontentloaded" | "commit"
                ) && v.trim_end_matches("ms").parse::<u64>().is_err()
                {
                    return Err(format!(
                        "--wait-until must be load, networkidle, domcontentloaded, commit or a number of ms, got {v:?}"
                    ));
                }
                out.wait_until = v;
            }
            "--timeout" => {
                out.timeout = value("--timeout")?
                    .parse()
                    .map_err(|_| "--timeout needs a number of seconds".to_owned())?
            }
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

    let ctx = lb_page::BrowserContext::new(lb_page::ContextOptions {
        persona,
        profile: None,
        tls_profile: Arc::new(profile),
        accept_invalid_certs: args.insecure,
        virtual_time: false,
    })
    .map_err(|e| e.to_string())?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting runtime: {e}"))?;
    let loaded = rt.block_on(load_page(&ctx, url, args))?;

    if args.verbose {
        eprintln!("{}", loaded.status);
        for (n, v) in &loaded.headers {
            eprintln!("{n}: {v}");
        }
        eprintln!();
    }
    eprintln!(
        "{} {} ({} bytes of HTML)",
        loaded.status,
        loaded.url,
        loaded.html.len()
    );

    let mut stdout = std::io::stdout().lock();
    let written = match args.dump {
        Dump::None => Ok(()),
        Dump::Html => stdout.write_all(loaded.html.as_bytes()),
        Dump::Text => stdout
            .write_all(loaded.text.as_bytes())
            .and_then(|()| stdout.write_all(b"\n")),
    };
    // A closed pipe (`| head`) is not an error for us.
    if let Err(e) = written.and_then(|()| stdout.flush())
        && e.kind() != std::io::ErrorKind::BrokenPipe
    {
        return Err(format!("writing output: {e}"));
    }

    let code = loaded.status;
    Ok(if (200..400).contains(&code) { 0 } else { 1 })
}

/// What `fetch` prints.
struct Loaded {
    status: u16,
    url: String,
    headers: Vec<(String, String)>,
    html: String,
    text: String,
}

/// Load the page and run its scripts (chrome build).
#[cfg(feature = "chrome")]
async fn load_page(
    ctx: &lb_page::BrowserContext,
    url: url::Url,
    args: &FetchArgs,
) -> Result<Loaded, String> {
    use std::time::Duration;
    let until: lb_page::WaitUntil = args.wait_until.parse()?;
    let browser = lb_page::Browser::new(lb_page::BrowserOptions {
        workers: 1,
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let page = browser.new_page(ctx).await.map_err(|e| e.to_string())?;
    let nav = page
        .navigate(url.as_str())
        .await
        .map_err(|e| e.to_string())?;
    match page
        .wait_for(until, Duration::from_secs(args.timeout))
        .await
    {
        Ok(()) => {}
        Err(lb_page::PageError::Timeout) => eprintln!(
            "warning: timed out waiting for {}; dumping the DOM as it is",
            args.wait_until
        ),
        Err(e) => return Err(e.to_string()),
    }
    let html = page.content().await.map_err(|e| e.to_string())?;
    let text = page.text().await.map_err(|e| e.to_string())?;
    for m in page.console_messages().await.unwrap_or_default() {
        if args.verbose || m.level == "error" {
            eprintln!("console.{}: {}", m.level, m.text);
        }
    }
    let _ = page.close().await;
    Ok(Loaded {
        status: nav.status,
        url: nav.url,
        headers: nav.headers,
        html,
        text,
    })
}

/// Fetch and parse without running scripts (firefox build: no
/// SpiderMonkey yet).
#[cfg(not(feature = "chrome"))]
async fn load_page(
    ctx: &lb_page::BrowserContext,
    url: url::Url,
    _args: &FetchArgs,
) -> Result<Loaded, String> {
    let (resp, doc) = lb_page::load_document(ctx, url)
        .await
        .map_err(|e| e.to_string())?;
    Ok(Loaded {
        status: resp.status,
        url: resp.url.to_string(),
        headers: resp.headers,
        html: doc.to_html(),
        text: lb_webapi::text::document_text(&doc),
    })
}

/// `serve` options.
#[derive(Debug)]
struct ServeArgs {
    port: u16,
    persona: Option<PathBuf>,
    allow_origins: Vec<String>,
}

fn parse_serve_args(args: &[String]) -> Result<ServeArgs, String> {
    let mut out = ServeArgs {
        port: 9222,
        persona: None,
        allow_origins: Vec::new(),
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
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
            "--port" => {
                out.port = value("--port")?
                    .parse()
                    .map_err(|_| "--port needs a port number".to_owned())?
            }
            "--persona" => out.persona = Some(PathBuf::from(value("--persona")?)),
            "--remote-allow-origins" => {
                out.allow_origins = value("--remote-allow-origins")?
                    .split(',')
                    .map(|s| s.trim().to_owned())
                    .filter(|s| !s.is_empty())
                    .collect()
            }
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(out)
}

/// Nearest `profiles/reference` directory above the current directory or
/// the executable.
fn find_reference_dir() -> Option<PathBuf> {
    let up = |start: &Path| {
        start
            .ancestors()
            .map(|d| d.join("profiles").join("reference"))
            .find(|d| d.is_dir())
    };
    std::env::current_dir()
        .ok()
        .and_then(|d| up(&d))
        .or_else(|| up(std::env::current_exe().ok()?.parent()?))
        .or_else(|| up(Path::new(env!("CARGO_MANIFEST_DIR"))))
}

/// The persona for `serve`: the given file (checked against the
/// consistency rules and the compiled family) or the family's reference
/// persona.
fn serve_persona(path: Option<&Path>) -> Result<lb_persona::PersonaProfile, String> {
    let path = match path {
        Some(p) => p.to_path_buf(),
        None => {
            let dir = find_reference_dir().ok_or("no profiles/reference directory found")?;
            match lb_js::COMPILED_FAMILY {
                Some(lb_persona::BrowserFamily::Firefox) => {
                    dir.join("firefox/win10_rtx3060_1080p.json")
                }
                _ => dir.join("chrome/chrome153_win10_rtx3060_1080p.json"),
            }
        }
    };
    let p = lb_persona::PersonaProfile::from_path_checked(&path)
        .map_err(|e| format!("loading {}: {e}", path.display()))?;
    if Some(p.browser.family) != lb_js::COMPILED_FAMILY {
        return Err(format!(
            "persona family {} does not match this build ({})",
            p.browser.family,
            lb_js::COMPILED_FAMILY.map_or("none", |f| f.as_str())
        ));
    }
    Ok(p)
}

fn run_serve(args: &ServeArgs) -> Result<(), String> {
    let persona = serve_persona(args.persona.as_deref())?;
    let persona_label = persona.profile_id.clone();
    let host = std::rc::Rc::new(lb_cdp::mock::MockHost::new(persona));
    let config = lb_cdp::ServerConfig {
        addr: std::net::SocketAddr::from(([127, 0, 0, 1], args.port)),
        allow_origins: args.allow_origins.clone(),
    };
    eprintln!(
        "page host: MOCK (lb-dom + lb-layout, no JS engine, loopback URLs only); persona {}",
        persona_label
    );
    lb_cdp::block_on_local(async move {
        let server = lb_cdp::Server::bind(config, host).await?;
        // Same line Chrome prints; launchers wait for it.
        eprintln!("\nDevTools listening on {}", server.browser_ws_url());
        server.run().await
    })
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Validate each persona file, print its report, and fail if any is invalid.
/// Works on personas of either family, whatever engine this build has.
fn run_validate(paths: &[String]) -> ExitCode {
    let mut all_ok = true;
    for path in paths {
        let outcome = lb_persona::validate_file(path);
        print!("{}", outcome.report);
        all_ok &= outcome.ok;
    }
    if all_ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::from(1)
    }
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
        Some("validate") => {
            let paths = &args[1..];
            if paths.iter().any(|a| a == "--help" || a == "-h") {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            if paths.is_empty() || paths.iter().any(|a| a.starts_with('-')) {
                eprintln!("error: validate takes one or more persona files\n");
                eprint!("{USAGE}");
                return ExitCode::from(2);
            }
            run_validate(paths)
        }
        Some("serve") => {
            if args[1..].iter().any(|a| a == "--help" || a == "-h") {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            let parsed = match parse_serve_args(&args[1..]) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}\n");
                    eprint!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match run_serve(&parsed) {
                Ok(()) => ExitCode::SUCCESS,
                Err(e) => {
                    eprintln!("error: {e}");
                    ExitCode::from(1)
                }
            }
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
        let p = a(&["u", "--wait-until", "networkidle", "--timeout=5"]).unwrap();
        assert_eq!((p.wait_until.as_str(), p.timeout), ("networkidle", 5));
        assert_eq!(a(&["u"]).unwrap().wait_until, "load");
        assert!(a(&["u", "--wait-until", "500"]).is_ok());
        assert!(a(&["u", "--wait-until", "soon"]).is_err());
    }

    #[test]
    fn parses_serve_args() {
        let a = |v: &[&str]| parse_serve_args(&v.iter().map(|s| s.to_string()).collect::<Vec<_>>());
        assert_eq!(a(&[]).unwrap().port, 9222);
        let p = a(&[
            "--port=0",
            "--persona",
            "p.json",
            "--remote-allow-origins",
            "http://a,http://b",
        ])
        .unwrap();
        assert_eq!(p.port, 0);
        assert_eq!(p.persona.as_deref(), Some(Path::new("p.json")));
        assert_eq!(p.allow_origins, ["http://a", "http://b"]);
        assert!(a(&["--port", "x"]).is_err());
        assert!(a(&["--bogus"]).is_err());
    }

    #[test]
    fn serve_persona_matches_the_build() {
        let p = serve_persona(None).unwrap();
        assert_eq!(Some(p.browser.family), lb_js::COMPILED_FAMILY);
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/reference");
        let other = match lb_js::COMPILED_FAMILY {
            Some(lb_persona::BrowserFamily::Chrome) => {
                root.join("firefox/win10_rtx3060_1080p.json")
            }
            _ => root.join("chrome/chrome153_win10_rtx3060_1080p.json"),
        };
        assert!(
            serve_persona(Some(&other))
                .unwrap_err()
                .contains("does not match")
        );
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
