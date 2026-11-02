//! `lb-fptest`: local TLS/H2 fingerprint capture. Test support, never shipped.
//!
//! ```text
//! lb-fptest capture --out profiles/tls/chrome_153_linux.json --listen 127.0.0.1:8443 --count 2 \
//!     --browser-version 153.0.8010.12
//! lb-fptest serve --listen 127.0.0.1:8443
//! ```
//!
//! Binds loopback only. Point a local browser or our own client at it.

use std::net::SocketAddr;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Parser, Subcommand};
use lb_fptest::clienthello::is_grease;
use lb_fptest::fingerprint::{Ja4, ja3};
use lb_fptest::profile::{
    ProfileMeta, build_profile, extension_order, is_resumption, utc_date_today,
};
use lb_fptest::server::{Capture, CaptureServer, ServerConfig};

#[derive(Parser)]
#[command(
    name = "lb-fptest",
    about = "Local TLS/H2 fingerprint capture (test support only)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Wait for N connections and write a profiles/tls/*.json profile.
    Capture {
        /// Output profile path; its file stem is the default profile_id.
        #[arg(long)]
        out: PathBuf,
        /// Loopback address to listen on.
        #[arg(long, default_value = "127.0.0.1:8443")]
        listen: SocketAddr,
        /// Connections to capture. Use 2+ to detect extension permutation.
        #[arg(long, default_value_t = 1)]
        count: usize,
        #[arg(long)]
        profile_id: Option<String>,
        /// Full browser version for the description, e.g. 153.0.8010.12.
        #[arg(long)]
        browser_version: Option<String>,
        /// Overrides the generated description.
        #[arg(long)]
        description: Option<String>,
    },
    /// Print a JSON summary (JA3, JA4, Akamai H2) for every connection.
    Serve {
        #[arg(long, default_value = "127.0.0.1:8443")]
        listen: SocketAddr,
        /// Stop after this many connections (0 = run until killed).
        #[arg(long, default_value_t = 0)]
        count: usize,
        /// ALPN to offer, comma separated (e.g. `http/1.1` to read headers
        /// in clear text).
        #[arg(long, default_value = "h2,http/1.1")]
        alpn: String,
    },
}

fn summary(c: &Capture) -> serde_json::Value {
    let ch = c.client_hello.as_ref();
    serde_json::json!({
        "peer": c.peer.to_string(),
        "error": c.error,
        "sni": ch.and_then(|h| h.server_name.clone()),
        "ja3": ch.map(ja3),
        "ja3_string": ch.map(lb_fptest::fingerprint::ja3_string),
        "ja4": ch.map(|h| Ja4::from_client_hello(h, 't').hashed()),
        "ja4_r": ch.map(|h| Ja4::from_client_hello(h, 't').raw()),
        "extensions": ch.map(|h| h.extension_ids().iter().map(|&e| if is_grease(e) { "GREASE".to_owned() } else { e.to_string() }).collect::<Vec<_>>()),
        "alpn": c.negotiated_alpn,
        "akamai_h2": c.h2.as_ref().map(|h| h.akamai()),
        "h2_frames": c.h2.as_ref().map(|h| h.frame_order.clone()),
        "h2_header_names": c.h2.as_ref().map(|h| h.header_names.iter().map(|n| n.clone().unwrap_or_else(|| "<huffman>".into())).collect::<Vec<_>>()),
        "h1": c.h1.as_ref().map(|h| serde_json::json!({ "request_line": h.request_line, "headers": h.headers })),
    })
}

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("lb-fptest: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), String> {
    let (listen, count) = match &cli.cmd {
        Cmd::Capture { listen, count, .. } | Cmd::Serve { listen, count, .. } => (*listen, *count),
    };
    let mut cfg = ServerConfig::default();
    if let Cmd::Serve { alpn, .. } = &cli.cmd {
        cfg.alpn = alpn
            .split(',')
            .map(|a| a.trim().as_bytes().to_vec())
            .collect();
    }
    if !listen.ip().is_loopback() {
        return Err(format!(
            "{listen} is not a loopback address; lb-fptest is local-only"
        ));
    }
    let (srv, mut rx) = CaptureServer::bind(&listen.to_string(), cfg)
        .await
        .map_err(|e| format!("bind {listen}: {e}"))?;
    eprintln!("lb-fptest: listening on https://{}/", srv.local_addr());

    let mut caps = Vec::new();
    while count == 0 || caps.len() < count {
        let Some(c) = rx.recv().await else { break };
        println!(
            "{}",
            serde_json::to_string(&summary(&c)).unwrap_or_default()
        );
        // Capture mode keeps only full handshakes that reached a request and
        // did not resume a session (resumption adds pre_shared_key).
        let complete = c.error.is_none() && (c.h2.is_some() || c.h1.is_some());
        let resumed = c.client_hello.as_ref().is_some_and(is_resumption);
        if matches!(cli.cmd, Cmd::Capture { .. }) && (!complete || resumed) {
            eprintln!("lb-fptest: skipping connection (complete={complete}, resumed={resumed})");
            continue;
        }
        caps.push(c);
    }

    let Cmd::Capture {
        out,
        profile_id,
        browser_version,
        description,
        ..
    } = cli.cmd
    else {
        return Ok(());
    };
    let profile_id = profile_id
        .or_else(|| out.file_stem().map(|s| s.to_string_lossy().into_owned()))
        .ok_or("cannot derive profile_id from --out")?;
    let description = description.unwrap_or_else(|| match &browser_version {
        Some(v) => format!("Chromium {v} headless on Linux, captured locally with lb-fptest"),
        None => "Captured locally with lb-fptest".into(),
    });
    for (i, c) in caps.iter().enumerate() {
        if let Some(ch) = &c.client_hello {
            eprintln!(
                "lb-fptest: capture {i} extension order {:?}",
                extension_order(ch)
            );
        }
    }
    let meta = ProfileMeta {
        profile_id,
        description,
        captured_date: utc_date_today(),
    };
    let built = build_profile(&caps, &meta)?;
    for n in &built.notes {
        eprintln!("lb-fptest: note: {n}");
    }
    eprintln!(
        "lb-fptest: permuted={} ja3={} ja4={} akamai={}",
        built.permuted, built.ja3, built.ja4, built.akamai
    );
    // The one disk write: the profile the user asked for with --out.
    std::fs::write(&out, &built.json).map_err(|e| format!("write {}: {e}", out.display()))?;
    eprintln!("lb-fptest: wrote {}", out.display());
    Ok(())
}
