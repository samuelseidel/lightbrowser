//! `lightbrowser mcp`: a Model Context Protocol server on stdio, so an AI
//! agent can drive the browser (ADR 0051).
//!
//! Newline-delimited JSON-RPC 2.0 on stdin and stdout; stdout carries
//! protocol messages only (logs go to stderr). The server listens on
//! nothing: the browser is driven through [`lb_cdp::InProcess`], channels
//! inside this process.
//!
//! - [`rpc`]: the protocol (initialize, ping, tools/list, tools/call)
//! - [`tools`]: tool names, descriptions and JSON Schemas
//! - [`policy`]: loopback-only navigation unless `--allow-host` says more
//! - [`snapshot`]: the accessibility tree as text with element refs
//! - [`keys`]: key names to trusted key events
//! - [`driver`] (chrome build): the CDP client behind the tools
#![cfg_attr(not(feature = "chrome"), allow(dead_code))]

use std::path::PathBuf;

#[cfg(feature = "chrome")]
mod driver;
mod keys;
mod policy;
mod rpc;
mod snapshot;
mod tools;

/// `mcp` options.
#[derive(Debug, Default, PartialEq)]
pub struct McpArgs {
    pub persona: Option<String>,
    pub profiles_dir: Option<PathBuf>,
    pub insecure: bool,
    pub proxy: Option<String>,
    pub proxy_bypass: Option<String>,
    /// Seconds a navigation or a single browser command may take.
    pub timeout: u64,
    /// `--allow-host` globs on top of the loopback set.
    pub allow_hosts: Vec<String>,
    /// `--log LEVEL`: browser log events to stderr.
    pub log: Option<tracing::Level>,
}

/// The longest `--timeout` (one day): a larger value overflows the clock.
const MAX_TIMEOUT_SECS: u64 = 24 * 60 * 60;

pub fn parse_args(args: &[String]) -> Result<McpArgs, String> {
    let mut out = McpArgs {
        timeout: 30,
        ..Default::default()
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
            "--persona" => out.persona = Some(value("--persona")?),
            "--profiles-dir" => out.profiles_dir = Some(PathBuf::from(value("--profiles-dir")?)),
            "--insecure" => out.insecure = true,
            "--proxy" => out.proxy = Some(value("--proxy")?),
            "--proxy-bypass-list" => out.proxy_bypass = Some(value("--proxy-bypass-list")?),
            "--allow-host" => {
                let g = value("--allow-host")?;
                if g.trim().is_empty() {
                    return Err("--allow-host needs a host or glob, such as *.corp.example".into());
                }
                out.allow_hosts.push(g);
            }
            "--timeout" => {
                out.timeout = value("--timeout")?
                    .parse()
                    .ok()
                    .filter(|t| (1..=MAX_TIMEOUT_SECS).contains(t))
                    .ok_or_else(|| {
                        format!("--timeout needs a number of seconds from 1 to {MAX_TIMEOUT_SECS}")
                    })?
            }
            "--log" => out.log = Some(super::ops::parse_log_level(&value("--log")?)?),
            other => return Err(format!("unknown option: {other}")),
        }
    }
    Ok(out)
}

/// Firefox build: no JS engine, nothing to drive.
#[cfg(not(feature = "chrome"))]
pub fn run(_args: &McpArgs) -> Result<(), String> {
    Err(
        "lightbrowser mcp needs the chrome build: the firefox build has no JS engine yet (M4b)"
            .into(),
    )
}

/// Chrome build: serve MCP on stdio until stdin closes.
#[cfg(feature = "chrome")]
pub fn run(args: &McpArgs) -> Result<(), String> {
    use std::sync::Arc;
    use std::time::Duration;

    let persona = super::select_persona(args.persona.as_deref())?;
    let tls_dir = match &args.profiles_dir {
        Some(d) => d.clone(),
        None => super::find_profiles_dir("tls").ok_or(
            "no profiles/tls directory found above the current directory or the executable; \
             pass --profiles-dir",
        )?,
    };
    // Fail fast when the persona and its TLS/H2 profile disagree.
    super::network_for(&persona, &tls_dir)?;
    let label = persona.profile_id.clone();

    let policy = policy::HostPolicy::new(&args.allow_hosts)?;
    let blocked = policy::BlockedLog::default();
    let mut opts = lb_page::CdpHostOptions::new(Arc::new(persona), tls_dir);
    opts.accept_invalid_certs = args.insecure;
    // One page: the main worker, and one spare for frames and workers.
    opts.workers = 2;
    if let Some(p) = &args.proxy {
        lb_net::Proxy::parse(p, args.proxy_bypass.as_deref())
            .map_err(|e| format!("--proxy: {e}"))?;
        opts.proxy_server = Some(p.clone());
        opts.proxy_bypass_list = args.proxy_bypass.clone();
    }
    // Transport-level policy (ADR 0044): loopback, `localhost` names, `*.test`
    // and the --allow-host entries. The same policy answers the tool's own
    // up-front check.
    opts.egress = Some(policy.egress());
    // Before the browser starts, so nothing it prints can reach the protocol.
    let out = protocol_out();
    super::ops::init_log(args.log);
    let host = lb_page::CdpHost::new(opts)?;
    let timeout = Duration::from_secs(args.timeout);
    eprintln!(
        "lightbrowser mcp {}: persona {label}; hosts allowed: loopback, localhost, *.localhost, *.test{}",
        env!("CARGO_PKG_VERSION"),
        args.allow_hosts
            .iter()
            .map(|h| format!(", {h}"))
            .collect::<String>()
    );
    lb_cdp::block_on_local(serve_stdio(host, timeout, policy, blocked, out))
        .map_err(|e| e.to_string())?
}

/// The stream protocol messages go to. On unix this is a private copy of
/// fd 1 and fd 1 itself is pointed at stderr, so nothing else in the
/// process (a library, a stray `println!`) can write into the protocol.
#[cfg(all(feature = "chrome", unix))]
fn protocol_out() -> Box<dyn std::io::Write> {
    use std::os::fd::FromRawFd;
    // SAFETY: `dup` returns a new descriptor we then own exclusively (it is
    // wrapped in a `File` once, here); `dup2(2, 1)` only repoints fd 1.
    unsafe {
        let fd = libc::dup(1);
        if fd < 0 {
            return Box::new(std::io::stdout());
        }
        libc::dup2(2, 1);
        Box::new(std::fs::File::from_raw_fd(fd))
    }
}

#[cfg(all(feature = "chrome", not(unix)))]
fn protocol_out() -> Box<dyn std::io::Write> {
    Box::new(std::io::stdout())
}

#[cfg(feature = "chrome")]
async fn serve_stdio(
    host: lb_page::CdpHost,
    timeout: std::time::Duration,
    policy: policy::HostPolicy,
    blocked: policy::BlockedLog,
    mut out: Box<dyn std::io::Write>,
) -> Result<(), String> {
    use std::io::BufRead;

    // SIGINT/SIGTERM end the server like a closed stdin (exit 0), even
    // during a tool call; a second signal, or the shutdown deadline,
    // exits at once.
    let mut stop = std::pin::pin!(super::ops::stop_signal());
    let driver = driver::Driver::new(host, timeout, policy, blocked)?;
    // A thread reads stdin lines so the browser's tasks keep running while
    // we wait; closing stdin ends the server.
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    std::thread::Builder::new()
        .name("mcp-stdin".into())
        .spawn(move || {
            let stdin = std::io::stdin();
            let mut lock = stdin.lock();
            let mut buf = Vec::new();
            loop {
                buf.clear();
                match lock.read_until(b'\n', &mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(_) => {
                        let line = String::from_utf8_lossy(&buf).into_owned();
                        if tx.send(line).is_err() {
                            break;
                        }
                    }
                }
            }
        })
        .map_err(|e| format!("starting the stdin reader: {e}"))?;
    loop {
        let line = tokio::select! {
            l = rx.recv() => l,
            _ = &mut stop => None,
        };
        let Some(line) = line else { break };
        // A signal also ends a tool call that is still running (a
        // navigation may wait up to --timeout).
        let reply = tokio::select! {
            r = rpc::handle_line(&driver, &line) => r,
            _ = &mut stop => break,
        };
        if let Some(reply) = reply
            && writeln!(out, "{reply}").and_then(|()| out.flush()).is_err()
        {
            // The client went away.
            break;
        }
    }
    driver.shutdown();
    // The process ends right after this: no teardown of V8 and the workers.
    std::mem::forget(driver);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(a: &[&str]) -> Vec<String> {
        a.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn parses_options() {
        let p = parse_args(&args(&[
            "--allow-host",
            "*.corp.example",
            "--allow-host=b.example",
            "--timeout",
            "5",
            "--persona=p.json",
            "--insecure",
            "--proxy",
            "http://127.0.0.1:3128",
            "--proxy-bypass-list=<local>",
            "--profiles-dir",
            "/x/tls",
        ]))
        .unwrap();
        assert_eq!(p.allow_hosts, ["*.corp.example", "b.example"]);
        assert_eq!(p.timeout, 5);
        assert_eq!(p.persona.as_deref(), Some("p.json"));
        assert!(p.insecure);
        assert_eq!(p.proxy.as_deref(), Some("http://127.0.0.1:3128"));
        assert_eq!(p.proxy_bypass.as_deref(), Some("<local>"));
        assert_eq!(p.profiles_dir, Some(PathBuf::from("/x/tls")));
        assert_eq!(parse_args(&[]).unwrap().timeout, 30);
    }

    #[test]
    fn rejects_bad_options() {
        assert!(parse_args(&args(&["--bogus"])).is_err());
        assert!(parse_args(&args(&["--allow-host"])).is_err());
        assert!(parse_args(&args(&["--allow-host", " "])).is_err());
        assert!(parse_args(&args(&["--timeout", "0"])).is_err());
        assert!(parse_args(&args(&["--timeout", "soon"])).is_err());
        // Used to be accepted and then panic on the first navigation.
        assert!(parse_args(&args(&["--timeout", "18446744073709551615"])).is_err());
        assert!(parse_args(&args(&["--timeout", "86401"])).is_err());
        assert_eq!(
            parse_args(&args(&["--timeout", "86400"])).unwrap().timeout,
            86400
        );
    }
}
