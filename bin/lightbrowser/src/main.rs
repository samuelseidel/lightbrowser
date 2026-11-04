//! `lightbrowser` CLI. Build per family:
//!   cargo build --release                                            # chrome
//!   cargo build --release --no-default-features --features firefox  # firefox

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lb_net::ClientHelloProfile;
use lb_persona::{Persona, PersonaProfile};

const USAGE: &str = "\
lightbrowser - headless browser for detection testing

USAGE:
  lightbrowser --version
  lightbrowser serve [--port 9222] [--persona FILE] [--remote-allow-origins O]
                    [--profiles-dir DIR] [--insecure] [--workers N]
                    [--proxy URL] [--proxy-bypass-list LIST]
                                            CDP server on 127.0.0.1 for Playwright
                                            connectOverCDP
  lightbrowser fetch <url> [OPTIONS]
  lightbrowser validate <persona.json>...   check v1/v2 personas against the
                                            consistency rules (exit 1 on any
                                            violation or load error)
  lightbrowser fingerprint-report [--persona P] [--url URL] [--profiles-dir DIR]
                    [--insecure] [--timeout SECS]
                                            load URL (default about:blank),
                                            compare the live fingerprint
                                            surface with the persona, print
                                            JSON (exit 1 on any mismatch)

FETCH OPTIONS:
  --dump html|text       print the DOM after scripts ran (html) or its text;
                         default: none
  --wait-until W         load (default) | networkidle | domcontentloaded |
                         commit | <ms> (load, then that many ms more)
  --timeout SECS         give up waiting after SECS (default 30)
  --persona P            a persona file (PATH.json), or chrome | firefox for
                         that family's reference persona (default: the
                         compiled family's reference persona)
  --profiles-dir DIR     TLS profiles (default: nearest profiles/tls above
                         the current directory or the executable)
  --insecure             accept invalid certificates (local test servers only)
  --proxy URL            proxy for every request: http://[user:pass@]host:port
                         (CONNECT for https/wss, forward for http) or
                         socks5://[user:pass@]host:port (remote DNS); as
                         Chrome's --proxy-server, also for `serve` (contexts
                         with their own proxyServer use theirs)
  --proxy-bypass-list L  Chrome's bypass rules (`*.a.com,10.0.0.0/8,<local>`);
                         loopback is never proxied unless L has <-loopback>
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
    proxy: Option<String>,
    proxy_bypass: Option<String>,
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
        proxy: None,
        proxy_bypass: None,
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
            "--proxy" => out.proxy = Some(value("--proxy")?),
            "--proxy-bypass-list" => out.proxy_bypass = Some(value("--proxy-bypass-list")?),
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

/// The reference persona of a family.
fn reference_persona_path(family: lb_persona::BrowserFamily) -> Result<PathBuf, String> {
    let dir = find_reference_dir().ok_or("no profiles/reference directory found")?;
    Ok(match family {
        lb_persona::BrowserFamily::Firefox => dir.join("firefox/win10_rtx3060_1080p.json"),
        lb_persona::BrowserFamily::Chrome => dir.join("chrome/chrome153_win10_rtx3060_1080p.json"),
    })
}

/// Load the full persona: a file, or `chrome` / `firefox` for that family's
/// reference persona (default: the compiled family). It is checked against
/// the consistency rules and must match the compiled engine family.
fn select_persona(name: Option<&str>) -> Result<PersonaProfile, String> {
    load_persona(name, true)
}

/// [`select_persona`], optionally without the consistency rules (only for
/// the hidden `serve --unchecked-persona`; the family must still match).
fn load_persona(name: Option<&str>, checked: bool) -> Result<PersonaProfile, String> {
    let compiled = lb_js::COMPILED_FAMILY.ok_or("this build has no JS engine family enabled")?;
    let path = match name {
        Some("chrome") => reference_persona_path(lb_persona::BrowserFamily::Chrome)?,
        Some("firefox") => reference_persona_path(lb_persona::BrowserFamily::Firefox)?,
        Some(path) => PathBuf::from(path),
        None => reference_persona_path(compiled)?,
    };
    let p = if checked {
        PersonaProfile::from_path_checked(&path)
    } else {
        PersonaProfile::from_path(&path)
    }
    .map_err(|e| format!("loading {}: {e}", path.display()))?;
    if p.browser.family != compiled {
        return Err(format!(
            "persona family {} does not match this build ({compiled}); rebuild with {}",
            p.browser.family,
            match p.browser.family {
                lb_persona::BrowserFamily::Chrome => "the default features",
                lb_persona::BrowserFamily::Firefox => "--no-default-features --features firefox",
            }
        ));
    }
    Ok(p)
}

/// The network persona of `profile` and the TLS/H2 profile it names, after
/// checking that they agree: same family, and the TLS profile's browser
/// major is the User-Agent's major. Fails fast otherwise.
fn network_for(
    profile: &PersonaProfile,
    tls_dir: &Path,
) -> Result<(Persona, ClientHelloProfile), String> {
    let net = profile.to_network();
    let path = tls_dir.join(format!("{}.json", net.tls_profile_id));
    let tls =
        ClientHelloProfile::load(&path).map_err(|e| format!("loading {}: {e}", path.display()))?;
    check_network(&net, &tls)?;
    Ok((net, tls))
}

fn check_network(net: &Persona, tls: &ClientHelloProfile) -> Result<(), String> {
    if net.family != lb_js::COMPILED_FAMILY.unwrap_or(net.family) {
        return Err(format!(
            "network persona family {} does not match this build",
            net.family
        ));
    }
    if tls.family() != Some(net.family) {
        return Err(format!(
            "TLS profile {} does not belong to the persona family {}",
            tls.profile_id, net.family
        ));
    }
    if tls.profile_id != net.tls_profile_id {
        return Err(format!(
            "TLS profile {} is not the persona's ({})",
            tls.profile_id, net.tls_profile_id
        ));
    }
    let tls_major = tls
        .profile_id
        .split('_')
        .nth(1)
        .and_then(|v| v.parse::<u32>().ok());
    let token = match net.family {
        lb_persona::BrowserFamily::Chrome => "Chrome/",
        lb_persona::BrowserFamily::Firefox => "Firefox/",
    };
    let ua_major = lb_persona::uach::ua_major(&net.user_agent, token);
    if let (Some(t), Some(u)) = (tls_major, ua_major)
        && t != u
    {
        return Err(format!(
            "TLS profile {} was captured from major {t}, but the User-Agent claims {u}",
            tls.profile_id
        ));
    }
    Ok(())
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
    let full = Arc::new(select_persona(args.persona.as_deref())?);
    let url = url::Url::parse(&args.url).map_err(|e| format!("invalid url {:?}: {e}", args.url))?;

    let dir = match &args.profiles_dir {
        Some(d) => d.clone(),
        None => find_profiles_dir().ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    let (persona, profile) = network_for(&full, &dir)?;

    let ctx = lb_page::BrowserContext::new(lb_page::ContextOptions {
        persona: Arc::new(persona),
        profile: Some(full),
        tls_profile: Arc::new(profile),
        accept_invalid_certs: args.insecure,
        virtual_time: false,
    })
    .map_err(|e| e.to_string())?;
    if let Some(p) = &args.proxy
        && let Some(p) = lb_net::Proxy::parse(p, args.proxy_bypass.as_deref())
            .map_err(|e| format!("--proxy: {e}"))?
    {
        ctx.set_proxy(Arc::new(p));
    }

    // The page and its network run on the browser's worker thread; this
    // runtime only waits for it. One page on one worker: its connections
    // can stay on that thread (no network thread, ADR 0018).
    ctx.connections().use_calling_thread();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting runtime: {e}"))?;
    let loaded = rt.block_on(load_page(&ctx, url, args))?;
    // One page per process: the process ends right after the output, so
    // the runtime is not shut down (nothing is pending; no disk state).
    std::mem::forget(rt);

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
        // One page per process: making the snapshot costs more than it saves.
        startup_snapshot: false,
        ..Default::default()
    })
    .map_err(|e| e.to_string())?;
    let page = browser
        .new_page_to_load(ctx)
        .await
        .map_err(|e| e.to_string())?;
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
    // No teardown: the process exits after printing (V8 isolate, DOM and
    // the worker thread go with it), like a browser's fast shutdown.
    std::mem::forget(page);
    std::mem::forget(browser);
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

/// `fingerprint-report` options.
#[derive(Debug, Default)]
struct ReportArgs {
    persona: Option<String>,
    url: Option<String>,
    profiles_dir: Option<PathBuf>,
    insecure: bool,
    timeout: u64,
}

fn parse_report_args(args: &[String]) -> Result<ReportArgs, String> {
    let mut out = ReportArgs {
        timeout: 30,
        ..Default::default()
    };
    let mut it = args.iter();
    while let Some(arg) = it.next() {
        let (flag, inline) = match arg.split_once('=') {
            Some((f, v)) if f.starts_with("--") => (f, Some(v.to_string())),
            _ => (arg.as_str(), None),
        };
        let mut value = |name: &str| -> Result<String, String> {
            inline
                .clone()
                .or_else(|| it.next().cloned())
                .ok_or_else(|| format!("{name} needs a value"))
        };
        match flag {
            "--persona" => out.persona = Some(value("--persona")?),
            "--url" => out.url = Some(value("--url")?),
            "--profiles-dir" => out.profiles_dir = Some(PathBuf::from(value("--profiles-dir")?)),
            "--timeout" => {
                out.timeout = value("--timeout")?
                    .parse()
                    .map_err(|_| "--timeout takes whole seconds".to_string())?
            }
            "--insecure" => out.insecure = true,
            other => return Err(format!("unknown option {other}")),
        }
    }
    Ok(out)
}

/// Load the page and print its fingerprint report; `Ok(false)` when a
/// surface contradicts the persona.
#[cfg(feature = "chrome")]
fn run_report(args: &ReportArgs) -> Result<bool, String> {
    use std::time::Duration;
    let full = Arc::new(select_persona(args.persona.as_deref())?);
    let url = args.url.clone().unwrap_or_else(|| "about:blank".into());
    let url = url::Url::parse(&url).map_err(|e| format!("invalid url {url:?}: {e}"))?;
    let dir = match &args.profiles_dir {
        Some(d) => d.clone(),
        None => find_profiles_dir().ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    let (persona, tls) = network_for(&full, &dir)?;
    let ctx = lb_page::BrowserContext::new(lb_page::ContextOptions {
        persona: Arc::new(persona),
        profile: Some(full),
        tls_profile: Arc::new(tls),
        accept_invalid_certs: args.insecure,
        virtual_time: false,
    })
    .map_err(|e| e.to_string())?;
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting runtime: {e}"))?;
    let report = rt.block_on(async {
        let browser = lb_page::Browser::new(lb_page::BrowserOptions {
            workers: 1,
            ..Default::default()
        })
        .map_err(|e| e.to_string())?;
        let page = browser.new_page(&ctx).await.map_err(|e| e.to_string())?;
        page.goto(
            url.as_str(),
            lb_page::WaitUntil::Load,
            Duration::from_secs(args.timeout),
        )
        .await
        .map_err(|e| e.to_string())?;
        let r = page.fingerprint_report().await.map_err(|e| e.to_string());
        let _ = page.close().await;
        r
    })?;
    let text = serde_json::to_string_pretty(&report).map_err(|e| e.to_string())?;
    println!("{text}");
    let s = &report["summary"];
    eprintln!(
        "fingerprint report for {}: {} ok, {} mismatch, {} unimplemented",
        report["persona"].as_str().unwrap_or("?"),
        s["ok"],
        s["mismatch"],
        s["unimplemented"]
    );
    Ok(report["ok"] == serde_json::Value::Bool(true))
}

#[cfg(not(feature = "chrome"))]
fn run_report(_args: &ReportArgs) -> Result<bool, String> {
    Err("fingerprint-report needs a JS engine; the firefox build has none yet (M4b)".into())
}

/// `serve` options.
#[derive(Debug)]
struct ServeArgs {
    port: u16,
    persona: Option<PathBuf>,
    /// Set by the hidden `--unchecked-persona FILE`: serve `persona` even if
    /// it breaks the consistency rules. Only for detect-bench tests with
    /// deliberately inconsistent (adversarial) personas; not in USAGE.
    unchecked: bool,
    allow_origins: Vec<String>,
    profiles_dir: Option<PathBuf>,
    insecure: bool,
    workers: Option<usize>,
    proxy: Option<String>,
    proxy_bypass: Option<String>,
}

fn parse_serve_args(args: &[String]) -> Result<ServeArgs, String> {
    let mut out = ServeArgs {
        port: 9222,
        persona: None,
        unchecked: false,
        allow_origins: Vec::new(),
        profiles_dir: None,
        insecure: false,
        workers: None,
        proxy: None,
        proxy_bypass: None,
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
            "--unchecked-persona" => {
                out.persona = Some(PathBuf::from(value("--unchecked-persona")?));
                out.unchecked = true;
            }
            "--profiles-dir" => out.profiles_dir = Some(PathBuf::from(value("--profiles-dir")?)),
            "--insecure" => out.insecure = true,
            "--proxy" => out.proxy = Some(value("--proxy")?),
            "--proxy-bypass-list" => out.proxy_bypass = Some(value("--proxy-bypass-list")?),
            "--workers" => {
                out.workers = Some(
                    value("--workers")?
                        .parse()
                        .map_err(|_| "--workers needs a number".to_owned())?,
                )
            }
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
fn serve_persona(path: Option<&Path>) -> Result<PersonaProfile, String> {
    select_persona(path.map(|p| p.to_str().unwrap_or_default()))
}

/// [`serve_persona`] without the consistency rules, for the hidden
/// `--unchecked-persona`. Violations are printed, not refused.
fn serve_unchecked_persona(path: &Path) -> Result<PersonaProfile, String> {
    let p = load_persona(Some(path.to_str().unwrap_or_default()), false)?;
    let report = lb_persona::consistency::validate(&p);
    if !report.is_valid() {
        let why = lb_persona::PersonaError::Inconsistent(report);
        eprintln!("warning: --unchecked-persona (bench tests only): {why}");
    }
    Ok(p)
}

fn run_serve(args: &ServeArgs) -> Result<(), String> {
    let persona = match (&args.persona, args.unchecked) {
        (Some(path), true) => serve_unchecked_persona(path)?,
        (path, _) => serve_persona(path.as_deref())?,
    };
    let tls_dir = match &args.profiles_dir {
        Some(d) => d.clone(),
        None => find_profiles_dir().ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    // Fail fast when the persona and its TLS/H2 profile disagree.
    network_for(&persona, &tls_dir)?;
    let config = lb_cdp::ServerConfig {
        addr: std::net::SocketAddr::from(([127, 0, 0, 1], args.port)),
        allow_origins: args.allow_origins.clone(),
    };
    serve_with(persona, tls_dir, args, config)
}

/// Chrome build: real pages (lb-page, V8).
#[cfg(feature = "chrome")]
fn serve_with(
    persona: PersonaProfile,
    tls_dir: PathBuf,
    args: &ServeArgs,
    config: lb_cdp::ServerConfig,
) -> Result<(), String> {
    let label = persona.profile_id.clone();
    let mut opts = lb_page::CdpHostOptions::new(Arc::new(persona), tls_dir);
    opts.accept_invalid_certs = args.insecure;
    if let Some(p) = &args.proxy {
        // Fail at start, not on the first page.
        lb_net::Proxy::parse(p, args.proxy_bypass.as_deref())
            .map_err(|e| format!("--proxy: {e}"))?;
        opts.proxy_server = Some(p.clone());
        opts.proxy_bypass_list = args.proxy_bypass.clone();
    }
    if let Some(n) = args.workers {
        opts.workers = n.max(1);
    }
    let host = std::rc::Rc::new(lb_page::CdpHost::new(opts)?);
    eprintln!("persona {label}");
    lb_cdp::block_on_local(async move {
        let server = lb_cdp::Server::bind(config, host).await?;
        // Same line Chrome prints; launchers wait for it.
        eprintln!("\nDevTools listening on {}", server.browser_ws_url());
        server.run().await
    })
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())
}

/// Firefox build: no SpiderMonkey yet (M4b), so `serve` runs the mock page
/// host (lb-dom + lb-layout, no JS engine, loopback URLs only).
#[cfg(not(feature = "chrome"))]
fn serve_with(
    persona: PersonaProfile,
    _tls_dir: PathBuf,
    _args: &ServeArgs,
    config: lb_cdp::ServerConfig,
) -> Result<(), String> {
    let label = persona.profile_id.clone();
    let host = std::rc::Rc::new(lb_cdp::mock::MockHost::new(persona));
    eprintln!("page host: MOCK (no JS engine in the firefox build yet); persona {label}");
    lb_cdp::block_on_local(async move {
        let server = lb_cdp::Server::bind(config, host).await?;
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
        Some("fingerprint-report") => {
            if args[1..].iter().any(|a| a == "--help" || a == "-h") {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            let parsed = match parse_report_args(&args[1..]) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}\n");
                    eprint!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match run_report(&parsed) {
                Ok(true) => ExitCode::SUCCESS,
                Ok(false) => ExitCode::from(1),
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
        assert!(!p.unchecked);
        assert!(a(&["--port", "x"]).is_err());
        assert!(a(&["--bogus"]).is_err());
        let u = a(&["--unchecked-persona", "bad.json"]).unwrap();
        assert!(u.unchecked);
        assert_eq!(u.persona.as_deref(), Some(Path::new("bad.json")));
    }

    #[test]
    fn unchecked_persona_skips_only_the_consistency_rules() {
        // An inconsistent persona is refused by --persona and served by
        // --unchecked-persona.
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/reference");
        let file = match lb_js::COMPILED_FAMILY {
            Some(lb_persona::BrowserFamily::Chrome) => "chrome/chrome153_win10_rtx3060_1080p.json",
            _ => "firefox/win10_rtx3060_1080p.json",
        };
        // A Windows UA with navigator.platform MacIntel breaks rule R01.
        let (from, to) = ("\"platform\": \"Win32\"", "\"platform\": \"MacIntel\"");
        let json = std::fs::read_to_string(root.join(file)).unwrap();
        assert!(json.contains(from), "{file} has no {from}");
        let dir = std::env::temp_dir().join(format!("lb-unchecked-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bad = dir.join("bad.json");
        std::fs::write(&bad, json.replacen(from, to, 1)).unwrap();
        let checked = serve_persona(Some(&bad));
        let unchecked = serve_unchecked_persona(&bad);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(checked.unwrap_err().contains("bad.json"));
        assert_eq!(
            unchecked.unwrap().browser.family,
            lb_js::COMPILED_FAMILY.unwrap()
        );
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
        assert!(
            select_persona(Some("p.json"))
                .unwrap_err()
                .contains("p.json")
        );
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

    #[test]
    fn network_persona_agrees_with_tls_profile() {
        let tls_dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../profiles/tls");
        let p = select_persona(None).unwrap();
        let (net, tls) = network_for(&p, &tls_dir).unwrap();
        assert_eq!(net.tls_profile_id, tls.profile_id);
        let mut bad = net.clone();
        bad.user_agent = bad
            .user_agent
            .replace("Chrome/153", "Chrome/120")
            .replace("Firefox/128", "Firefox/115");
        assert!(check_network(&bad, &tls).unwrap_err().contains("major"));
    }
}
