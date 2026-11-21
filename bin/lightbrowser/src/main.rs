//! `lightbrowser` CLI. Build per family:
//!   cargo build --release                                            # chrome
//!   cargo build --release --no-default-features --features firefox  # firefox

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;

use lb_net::ClientHelloProfile;
use lb_persona::{Persona, PersonaProfile};

mod mcp;
mod ops;

const USAGE: &str = "\
lightbrowser - headless browser for detection testing

USAGE:
  lightbrowser --version [--verbose]        --verbose adds the commit id
  lightbrowser serve [--port 9222] [--persona FILE] [--remote-allow-origins O]
                    [--profiles-dir DIR] [--insecure] [--workers N]
                    [--proxy URL] [--proxy-bypass-list LIST]
                    [--tcp-profile NAME]
                    [--egress open|loopback|allowlist] [--allow-host H]...
                    [--max-pages N] [--max-connections N] [--log LEVEL]
                                            CDP server on 127.0.0.1 for Playwright
                                            connectOverCDP. --max-pages: open
                                            pages at most (default 1000; more
                                            fail with a CDP error);
                                            --max-connections: client
                                            connections at most (default 512;
                                            more get HTTP 503). /json/new
                                            needs PUT, as in Chrome (GET gets
                                            405). SIGINT/SIGTERM close pages
                                            and exit 0; a second signal, or 5 s
                                            without finishing, exits at once
                                            (status 130). Same for mcp.
  lightbrowser mcp [--allow-host ENTRY]... [--persona FILE] [--profiles-dir DIR]
                  [--insecure] [--proxy URL] [--proxy-bypass-list LIST]
                  [--timeout SECS] [--log LEVEL]
                                            Model Context Protocol server on
                                            stdin/stdout (JSON-RPC lines) so an
                                            AI agent can drive the browser;
                                            listens on nothing. Only loopback,
                                            localhost, *.localhost and *.test
                                            are visited unless --allow-host adds
                                            hosts (repeatable; the same entries
                                            as `serve`, see --allow-host below).
                                            See ADR 0051.
  lightbrowser fetch <url> [OPTIONS]
  lightbrowser tcp-shim [--queue N] [--tcp-dir DIR] [--no-rules]   (Linux, root)
  lightbrowser validate <persona.json>...   check v1/v2 personas against the
                                            consistency rules (exit 1 on any
                                            violation or load error)
  lightbrowser fingerprint-report [--persona P] [--url URL] [--profiles-dir DIR]
                    [--insecure] [--timeout SECS]
                    [--egress open|loopback|allowlist] [--allow-host H]...
                                            load URL (default about:blank),
                                            compare the live fingerprint
                                            surface with the persona, print
                                            JSON (exit 1 on any mismatch)

FETCH OPTIONS:
  --dump MODE            print the DOM after scripts ran as html, its text,
                         markdown, its links (URL<TAB>text per line) or a json
                         summary (url, status, title, headings, links, text,
                         markdown); default: none
  --wait-until W         load (default) | networkidle | domcontentloaded |
                         commit | <ms> (load, then that many ms more)
  --timeout SECS         give up after SECS, waiting for the response
                         included (default 30)
  --persona P            a persona file (PATH.json), or chrome | firefox for
                         that family's reference persona (default: the
                         compiled family's reference persona)
  --profiles-dir DIR     TLS profiles (default: nearest profiles/tls above
                         the current directory or the executable)
  --tcp-profile NAME     shape the TCP SYN like a Windows, macOS or Linux host:
                         persona (the persona's tcp_profile_id) | off | a
                         profile id from profiles/tcp. Default: off. Windows and
                         macOS profiles need `tcp-shim` running (ADR 0031).
                         Also accepted by `serve` (every CDP context; T10).
  --insecure             accept invalid certificates (local test servers only)
  --proxy URL            proxy for every request: http://[user:pass@]host:port
                         (CONNECT for https/wss, forward for http) or
                         socks5://[user:pass@]host:port (remote DNS); as
                         Chrome's --proxy-server, also for `serve` (contexts
                         with their own proxyServer use theirs)
  --proxy-bypass-list L  Chrome's bypass rules (`*.a.com,10.0.0.0/8,<local>`);
                         loopback is never proxied unless L has <-loopback>
  --egress MODE          which destinations may be connected to (also for
                         `serve`): open (default, no checks) | loopback
                         (127.0.0.0/8, ::1, localhost, *.localhost, and the
                         --allow-host entries) | allowlist (only the entries).
                         A refused request fails as net::ERR_BLOCKED_BY_CLIENT
                         and sends nothing: a name that is not allowed is not
                         even looked up; every resolved address is checked.
  --allow-host H         add an allowed destination (repeatable): a name glob
                         (*.lab.test), an IP (10.0.0.5, [fd00::1]) or a CIDR
                         block (10.0.0.0/8). Needs --egress loopback|allowlist.
                         A proxy's own address must be allowed too.
  -v, --verbose          print the status line and headers to stderr

SERVE AND MCP OPTIONS:
  --log LEVEL            print the browser's log events at LEVEL and above to
                         stderr: error | warn | info | debug (default: none)

TCP-SHIM OPTIONS:
  --queue N              netfilter queue number (default 4100)
  --tcp-dir DIR          TCP profiles (default: nearest profiles/tcp)
  --no-rules             do not install the iptables rule; you manage it
  Rewrites the SYN, IP ID and SYN retransmissions of connections marked by
  `fetch`/`serve --tcp-profile`. Needs CAP_NET_ADMIN and CAP_NET_RAW. If it
  stops, marked connections fail: they never leak the host's own stack.
  Runs until Ctrl-C.

Exit code: 0 on 2xx/3xx, 1 on other statuses or errors, 2 on bad usage.
";

/// `--egress` and `--allow-host` (ADR 0044), shared by `fetch` and `serve`.
#[derive(Debug, Default)]
struct EgressArgs {
    mode: Option<String>,
    allow: Vec<String>,
}

impl EgressArgs {
    /// `--egress MODE`. Two different modes are a usage error (the last
    /// one must not silently win and lift a restriction).
    fn set_mode(&mut self, mode: String) -> Result<(), String> {
        match &self.mode {
            Some(prev) if !prev.trim().eq_ignore_ascii_case(mode.trim()) => Err(format!(
                "--egress given twice with different values ({prev}, {mode})"
            )),
            _ => {
                self.mode = Some(mode);
                Ok(())
            }
        }
    }

    /// The policy these flags ask for; `None` for the default (open).
    fn policy(&self) -> Result<Option<Arc<lb_net::EgressPolicy>>, String> {
        let mode = self.mode.as_deref().unwrap_or("open");
        let policy = lb_net::EgressPolicy::parse(mode, &self.allow).map_err(|e| {
            if self.allow.is_empty() {
                format!("--egress: {e}")
            } else {
                format!("--egress/--allow-host: {e} (pass --egress loopback or allowlist)")
            }
        })?;
        Ok((!policy.is_open()).then(|| Arc::new(policy)))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Dump {
    None,
    Html,
    Text,
    Markdown,
    Links,
    Json,
}

impl Dump {
    /// The modes made by `lb_page::extract`.
    fn extracts(self) -> bool {
        matches!(self, Dump::Markdown | Dump::Links | Dump::Json)
    }
}

#[derive(Debug)]
struct FetchArgs {
    url: String,
    dump: Dump,
    wait_until: String,
    timeout: u64,
    persona: Option<String>,
    profiles_dir: Option<PathBuf>,
    tcp_profile: Option<String>,
    insecure: bool,
    verbose: bool,
    proxy: Option<String>,
    proxy_bypass: Option<String>,
    egress: EgressArgs,
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
        tcp_profile: None,
        insecure: false,
        verbose: false,
        proxy: None,
        proxy_bypass: None,
        egress: EgressArgs::default(),
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
                    "markdown" => Dump::Markdown,
                    "links" => Dump::Links,
                    "json" => Dump::Json,
                    other => {
                        return Err(format!(
                            "--dump must be one of html, text, markdown, links, json, got {other:?}"
                        ));
                    }
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
            "--tcp-profile" => out.tcp_profile = Some(value("--tcp-profile")?),
            "--insecure" => out.insecure = true,
            "--proxy" => out.proxy = Some(value("--proxy")?),
            "--proxy-bypass-list" => out.proxy_bypass = Some(value("--proxy-bypass-list")?),
            "--egress" => out.egress.set_mode(value("--egress")?)?,
            "--allow-host" => out.egress.allow.push(value("--allow-host")?),
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
    out.egress.policy()?;
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

/// Nearest `profiles/<kind>` directory at or above `start`.
fn find_profiles_upward(start: &Path, kind: &str) -> Option<PathBuf> {
    start
        .ancestors()
        .map(|d| d.join("profiles").join(kind))
        .find(|d| d.is_dir())
}

/// `profiles/<kind>` above the current directory or the executable.
fn find_profiles_dir(kind: &str) -> Option<PathBuf> {
    std::env::current_dir()
        .ok()
        .and_then(|d| find_profiles_upward(&d, kind))
        .or_else(|| {
            let exe = std::env::current_exe().ok()?;
            find_profiles_upward(exe.parent()?, kind)
        })
}

fn run_fetch(args: &FetchArgs) -> Result<i32, String> {
    let full = Arc::new(select_persona(args.persona.as_deref())?);
    let url = url::Url::parse(&args.url).map_err(|e| format!("invalid url {:?}: {e}", args.url))?;

    let dir = match &args.profiles_dir {
        Some(d) => d.clone(),
        None => find_profiles_dir("tls").ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    let (persona, profile) = network_for(&full, &dir)?;

    let tcp_id = match args.tcp_profile.as_deref() {
        None | Some("off") => None,
        Some("persona") => Some(persona.tcp_profile_id.clone()),
        Some(id) => Some(id.to_owned()),
    };

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
    if let Some(policy) = args.egress.policy()? {
        ctx.set_egress(policy);
    }
    if let Some(id) = tcp_id {
        // profiles/tcp sits next to profiles/tls.
        let tcp_dir = args
            .profiles_dir
            .as_ref()
            .and_then(|d| d.parent().map(|p| p.join("tcp")))
            .or_else(|| find_profiles_dir("tcp"))
            .ok_or("no profiles/tcp directory found; pass --profiles-dir")?;
        let path = tcp_dir.join(format!("{id}.json"));
        let tcp = lb_net::tcpfp::TcpProfile::load(&path)
            .map_err(|e| format!("loading {}: {e}", path.display()))?;
        ctx.set_tcp_profile(Arc::new(tcp))
            .map_err(|e| e.to_string())?;
    }

    // The page and its network run on the browser's worker thread; this
    // runtime only waits for it. One page on one worker: its connections
    // can stay on that thread (no network thread, ADR 0018).
    ctx.connections().use_calling_thread();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("starting runtime: {e}"))?;
    let requested = url.to_string();
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
        Dump::Markdown | Dump::Links | Dump::Json => match &loaded.extracted {
            Some(e) => stdout.write_all(
                match args.dump {
                    Dump::Markdown => e.markdown.clone(),
                    Dump::Links => lb_page::extract::links_dump(e),
                    _ => lb_page::extract::json_dump(e, &requested, &loaded.url, loaded.status),
                }
                .as_bytes(),
            ),
            None => return Err("nothing was extracted".into()),
        },
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
    /// Set for the `--dump markdown | links | json` modes.
    extracted: Option<lb_page::extract::Extracted>,
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
    // `--timeout` bounds the whole load: the navigation up to commit, then
    // the wait for `--wait-until`.
    let now = tokio::time::Instant::now();
    let deadline = now
        .checked_add(Duration::from_secs(args.timeout))
        .unwrap_or_else(|| now + Duration::from_secs(100 * 365 * 24 * 3600));
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
    let nav = match tokio::time::timeout_at(deadline, page.navigate(url.as_str())).await {
        Ok(r) => r.map_err(|e| e.to_string())?,
        Err(_) => {
            // The navigation is still running on the worker thread; the
            // process exits right after this error, so do not wait for it.
            std::mem::forget(page);
            std::mem::forget(browser);
            return Err(format!(
                "timed out after {} s waiting for a response from {url}",
                args.timeout
            ));
        }
    };
    let left = deadline.saturating_duration_since(tokio::time::Instant::now());
    match page.wait_for(until, left).await {
        Ok(()) => {}
        Err(lb_page::PageError::Timeout) => eprintln!(
            "warning: timed out waiting for {}; dumping the DOM as it is",
            args.wait_until
        ),
        Err(e) => return Err(e.to_string()),
    }
    let html = page.content().await.map_err(|e| e.to_string())?;
    let text = page.text().await.map_err(|e| e.to_string())?;
    let extracted = if args.dump.extracts() {
        Some(page.extract().await.map_err(|e| e.to_string())?)
    } else {
        None
    };
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
        extracted,
    })
}

/// Fetch and parse without running scripts (firefox build: no
/// SpiderMonkey yet).
#[cfg(not(feature = "chrome"))]
async fn load_page(
    ctx: &lb_page::BrowserContext,
    url: url::Url,
    args: &FetchArgs,
) -> Result<Loaded, String> {
    let timeout = std::time::Duration::from_secs(args.timeout);
    let (resp, doc) =
        match tokio::time::timeout(timeout, lb_page::load_document(ctx, url.clone())).await {
            Ok(r) => r.map_err(|e| e.to_string())?,
            Err(_) => {
                return Err(format!(
                    "timed out after {} s waiting for a response from {url}",
                    args.timeout
                ));
            }
        };
    let text = lb_webapi::text::document_text(&doc);
    // No style engine here: only `hidden` and inline styles hide content.
    let extracted = args
        .dump
        .extracts()
        .then(|| lb_page::extract::extract_document(&doc, &resp.url, text.clone()));
    Ok(Loaded {
        status: resp.status,
        url: resp.url.to_string(),
        headers: resp.headers,
        html: doc.to_html(),
        text,
        extracted,
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
    egress: EgressArgs,
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
            "--egress" => out.egress.set_mode(value("--egress")?)?,
            "--allow-host" => out.egress.allow.push(value("--allow-host")?),
            other => return Err(format!("unknown option {other}")),
        }
    }
    out.egress.policy()?;
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
        None => find_profiles_dir("tls").ok_or(
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
    if let Some(policy) = args.egress.policy()? {
        ctx.set_egress(policy);
    }
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
    /// Same as `fetch --tcp-profile` (T10): persona | off | profile id.
    tcp_profile: Option<String>,
    egress: EgressArgs,
    /// `--max-pages`, `--max-connections`, `--log` (GAP-096, GAP-097).
    max_pages: Option<usize>,
    max_connections: Option<usize>,
    log: Option<tracing::Level>,
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
        tcp_profile: None,
        egress: EgressArgs::default(),
        max_pages: None,
        max_connections: None,
        log: None,
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
            "--tcp-profile" => out.tcp_profile = Some(value("--tcp-profile")?),
            "--egress" => out.egress.set_mode(value("--egress")?)?,
            "--allow-host" => out.egress.allow.push(value("--allow-host")?),
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
            "--max-pages" | "--max-connections" => {
                let n = value(flag)?
                    .parse::<usize>()
                    .ok()
                    .filter(|n| *n > 0)
                    .ok_or_else(|| format!("{flag} needs a number above 0"))?;
                match flag {
                    "--max-pages" => out.max_pages = Some(n),
                    _ => out.max_connections = Some(n),
                }
            }
            "--log" => out.log = Some(ops::parse_log_level(&value("--log")?)?),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    out.egress.policy()?;
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
        None => find_profiles_dir("tls").ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    // Fail fast when the persona and its TLS/H2 profile disagree.
    network_for(&persona, &tls_dir)?;
    let config = lb_cdp::ServerConfig {
        addr: std::net::SocketAddr::from(([127, 0, 0, 1], args.port)),
        allow_origins: args.allow_origins.clone(),
        max_pages: args.max_pages.unwrap_or(lb_cdp::DEFAULT_MAX_PAGES),
        max_connections: args
            .max_connections
            .unwrap_or(lb_cdp::DEFAULT_MAX_CONNECTIONS),
        ..lb_cdp::ServerConfig::default()
    };
    ops::init_log(args.log);
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
    // Validate TLS/H2 again so we can run the same TCP OS check as fetch.
    let (net_persona, tls_profile) = network_for(&persona, &tls_dir)?;
    let tcp_id_default = net_persona.tcp_profile_id.clone();
    let mut opts = lb_page::CdpHostOptions::new(Arc::new(persona), tls_dir);
    opts.accept_invalid_certs = args.insecure;
    if let Some(p) = &args.proxy {
        // Fail at start, not on the first page.
        lb_net::Proxy::parse(p, args.proxy_bypass.as_deref())
            .map_err(|e| format!("--proxy: {e}"))?;
        opts.proxy_server = Some(p.clone());
        opts.proxy_bypass_list = args.proxy_bypass.clone();
    }
    if let Some(t) = &args.tcp_profile {
        // Fail at start when the profile is missing, OS-mismatched, or the
        // shim rule is absent (Windows/macOS on Linux).
        opts.tcp_profile = Some(t.clone());
        if t != "off" {
            let id = if t == "persona" {
                tcp_id_default.as_str()
            } else {
                t.as_str()
            };
            let tcp_dir = args
                .profiles_dir
                .as_ref()
                .and_then(|d| d.parent().map(|p| p.join("tcp")))
                .or_else(|| find_profiles_dir("tcp"))
                .ok_or("no profiles/tcp directory found; pass --profiles-dir")?;
            let path = tcp_dir.join(format!("{id}.json"));
            let tcp = lb_net::tcpfp::TcpProfile::load(&path)
                .map_err(|e| format!("loading {}: {e}", path.display()))?;
            lb_net::fetch::NetContext::new(Arc::new(net_persona), Arc::new(tls_profile))
                .and_then(|n| n.with_tcp_profile(Arc::new(tcp)))
                .map_err(|e| format!("--tcp-profile: {e}"))?;
        }
    }
    if let Some(n) = args.workers {
        opts.workers = n.max(1);
    }
    opts.egress = args.egress.policy()?;
    let host = std::rc::Rc::new(lb_page::CdpHost::new(opts)?);
    eprintln!("persona {label}");
    let stop = ops::stop_signal();
    // On SIGINT/SIGTERM `run_until` closes pages and contexts; dropping the
    // host at the end of `block_on_local` joins the worker threads.
    lb_cdp::block_on_local(async move {
        let server = lb_cdp::Server::bind(config, host).await?;
        // Same line Chrome prints; launchers wait for it.
        eprintln!("\nDevTools listening on {}", server.browser_ws_url());
        server.run_until(stop).await
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
    args: &ServeArgs,
    config: lb_cdp::ServerConfig,
) -> Result<(), String> {
    // The mock page host loads loopback URLs only, which is at least as
    // strict as `open` and `loopback`; an allowlist could not be honoured
    // (it would not even allow all of loopback), so it is refused rather
    // than accepted and ignored (ADR 0044).
    if let Some(p) = args.egress.policy()?
        && p.mode() == lb_net::EgressMode::AllowList
    {
        return Err(
            "--egress allowlist is not supported by the firefox build's serve (its mock page \
             host loads loopback URLs only)"
                .into(),
        );
    }
    let label = persona.profile_id.clone();
    let host = std::rc::Rc::new(lb_cdp::mock::MockHost::new(persona));
    eprintln!("page host: MOCK (no JS engine in the firefox build yet); persona {label}");
    let stop = ops::stop_signal();
    lb_cdp::block_on_local(async move {
        let server = lb_cdp::Server::bind(config, host).await?;
        eprintln!("\nDevTools listening on {}", server.browser_ws_url());
        server.run_until(stop).await
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
    ops::install_panic_hook();
    match args.first().map(String::as_str) {
        Some("--version" | "-V") => {
            ops::print_version(args[1..].iter().any(|a| a == "--verbose" || a == "-v"));
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
        Some("tcp-shim") => match tcp_shim::run(&args[1..]) {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("error: {e}");
                ExitCode::from(1)
            }
        },
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
        Some("mcp") => {
            if args[1..].iter().any(|a| a == "--help" || a == "-h") {
                print!("{USAGE}");
                return ExitCode::SUCCESS;
            }
            let parsed = match mcp::parse_args(&args[1..]) {
                Ok(p) => p,
                Err(e) => {
                    eprintln!("error: {e}\n");
                    eprint!("{USAGE}");
                    return ExitCode::from(2);
                }
            };
            match mcp::run(&parsed) {
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

/// `lightbrowser tcp-shim`: the privileged packet shim (ADR 0031).
mod tcp_shim {
    #[cfg(target_os = "linux")]
    pub fn run(args: &[String]) -> Result<(), String> {
        use std::sync::atomic::{AtomicBool, Ordering};

        use lb_net::tcpfp::TcpProfile;
        use lb_net::tcpfp::nfq::{RuleGuard, Shim};

        // Set by the signal handler; the shim polls it. Process control only,
        // not fingerprint data.
        static STOP: AtomicBool = AtomicBool::new(false);
        extern "C" fn on_signal(_: libc::c_int) {
            STOP.store(true, Ordering::Relaxed);
        }

        let (mut queue, mut dir, mut rules) = (4100u16, None, true);
        let mut it = args.iter();
        while let Some(a) = it.next() {
            match a.as_str() {
                "--queue" => {
                    queue = it
                        .next()
                        .and_then(|v| v.parse().ok())
                        .ok_or("--queue needs a number from 0 to 65535")?;
                }
                "--tcp-dir" => dir = it.next().map(std::path::PathBuf::from),
                "--no-rules" => rules = false,
                other => return Err(format!("unknown option: {other}")),
            }
        }
        let dir = dir
            .or_else(|| super::find_profiles_dir("tcp"))
            .ok_or("no profiles/tcp directory found; pass --tcp-dir")?;
        let profiles = TcpProfile::load_dir(&dir).map_err(|e| e.to_string())?;
        let shim = Shim::bind(queue, profiles.clone()).map_err(|e| {
            format!("cannot bind netfilter queue {queue} (needs CAP_NET_ADMIN, and the queue must be free): {e}")
        })?;
        let _rules = if rules {
            Some(
                RuleGuard::install(queue)
                    .map_err(|e| format!("installing the iptables rule: {e}"))?,
            )
        } else {
            None
        };
        // SAFETY: the handler only stores to an atomic, which is
        // async-signal-safe.
        unsafe {
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
        eprintln!(
            "tcp-shim: queue {queue}, {} profiles ({}), Ctrl-C to stop",
            profiles.len(),
            profiles
                .iter()
                .map(|p| p.profile_id.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        let stats = shim.run(&STOP).map_err(|e| e.to_string())?;
        eprintln!("tcp-shim: stopped. {stats:?}");
        Ok(())
    }

    #[cfg(not(target_os = "linux"))]
    pub fn run(_: &[String]) -> Result<(), String> {
        Err("tcp-shim needs Linux".into())
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
            "--tcp-profile",
            "persona",
        ])
        .unwrap();
        assert_eq!(p.tcp_profile.as_deref(), Some("persona"));
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
    fn parses_egress_flags() {
        let strs = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        // Default: open, no policy.
        let f = parse_fetch_args(&strs(&["http://x/"])).unwrap();
        assert!(f.egress.policy().unwrap().is_none());
        let f = parse_fetch_args(&strs(&["http://x/", "--egress", "open"])).unwrap();
        assert!(f.egress.policy().unwrap().is_none());
        // Loopback with entries, both flag spellings, for fetch and serve.
        let f = parse_fetch_args(&strs(&[
            "http://x/",
            "--egress=loopback",
            "--allow-host",
            "10.0.0.0/8",
            "--allow-host=*.lab.test",
        ]))
        .unwrap();
        let p = f.egress.policy().unwrap().unwrap();
        assert_eq!(p.mode(), lb_net::EgressMode::Loopback);
        assert!(p.check_name("10.1.1.1").is_ok() && p.check_name("a.lab.test").is_ok());
        assert!(p.check_name("example.com").is_err());
        let s =
            parse_serve_args(&strs(&["--egress", "allowlist", "--allow-host", "[::1]"])).unwrap();
        let p = s.egress.policy().unwrap().unwrap();
        assert_eq!(p.mode(), lb_net::EgressMode::AllowList);
        assert!(p.check_name("127.0.0.1").is_err() && p.check_name("::1").is_ok());
        // Mistakes are usage errors.
        assert!(parse_fetch_args(&strs(&["u", "--egress", "closed"])).is_err());
        assert!(parse_fetch_args(&strs(&["u", "--allow-host", "a.test"])).is_err());
        assert!(parse_serve_args(&strs(&["--egress", "open", "--allow-host", "a.test"])).is_err());
        assert!(
            parse_serve_args(&strs(&["--egress", "loopback", "--allow-host", "a:80"])).is_err()
        );
        assert!(parse_serve_args(&strs(&["--egress"])).is_err());
        // Two different modes must not let the last one lift the first.
        assert!(
            parse_fetch_args(&strs(&["u", "--egress", "loopback", "--egress", "open"])).is_err()
        );
        assert!(parse_serve_args(&strs(&["--egress=open", "--egress", "loopback"])).is_err());
        assert!(parse_serve_args(&strs(&["--egress=loopback", "--egress", "Loopback"])).is_ok());
        // An entry that holds every address is refused like `*`.
        for e in ["0.0.0.0/0", "::/0", "*"] {
            assert!(
                parse_serve_args(&strs(&["--egress", "loopback", "--allow-host", e])).is_err(),
                "{e}"
            );
        }
        // A name pinned to addresses.
        let s = parse_serve_args(&strs(&[
            "--egress",
            "loopback",
            "--allow-host",
            "a.lab.test@10.0.0.0/8",
        ]))
        .unwrap();
        assert!(
            s.egress
                .policy()
                .unwrap()
                .unwrap()
                .check_name("a.lab.test")
                .is_ok()
        );
        // `fingerprint-report` takes the same flags.
        let r = parse_report_args(&strs(&["--url", "http://x/", "--egress", "loopback"])).unwrap();
        assert!(r.egress.policy().unwrap().is_some());
        assert!(parse_report_args(&strs(&["--allow-host", "a.test"])).is_err());
        assert!(parse_report_args(&strs(&["--egress", "nope"])).is_err());
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
        let t = a(&["--tcp-profile", "persona"]).unwrap();
        assert_eq!(t.tcp_profile.as_deref(), Some("persona"));
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
