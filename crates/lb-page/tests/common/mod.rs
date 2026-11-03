//! Test support: a loopback HTTP/1.1 server and a browser with the
//! default Chrome persona.
#![allow(dead_code, clippy::type_complexity)]

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use lb_net::ClientHelloProfile;
use lb_page::{Browser, BrowserContext, BrowserOptions, ContextOptions, Page, WaitUntil};
use lb_persona::Persona;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// One served resource.
#[derive(Clone)]
pub struct Resource {
    pub content_type: String,
    pub body: Vec<u8>,
    pub headers: Vec<(String, String)>,
    pub status: u16,
    /// Delay before answering.
    pub delay_ms: u64,
}

impl Resource {
    pub fn new(ct: &str, body: impl Into<Vec<u8>>) -> Resource {
        Resource {
            content_type: ct.into(),
            body: body.into(),
            headers: Vec::new(),
            status: 200,
            delay_ms: 0,
        }
    }
    pub fn html(body: &str) -> Resource {
        Self::new("text/html; charset=utf-8", body)
    }
    pub fn js(body: &str) -> Resource {
        Self::new("text/javascript", body)
    }
    pub fn header(mut self, k: &str, v: &str) -> Resource {
        self.headers.push((k.into(), v.into()));
        self
    }
    pub fn delay(mut self, ms: u64) -> Resource {
        self.delay_ms = ms;
        self
    }
}

/// A loopback server. Unknown paths give 404. Requests are logged.
pub struct Server {
    pub port: u16,
    pub log: Arc<Mutex<Vec<(String, String, Vec<(String, String)>, Vec<u8>)>>>,
}

impl Server {
    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{}", self.port, path)
    }
    pub fn requests(&self) -> Vec<(String, String, Vec<(String, String)>, Vec<u8>)> {
        self.log.lock().unwrap().clone()
    }
}

/// Serve fixed routes (plus `/echo`).
pub async fn serve(routes: Vec<(&str, Resource)>) -> Server {
    let routes: HashMap<String, Resource> =
        routes.into_iter().map(|(p, r)| (p.to_owned(), r)).collect();
    serve_fn(Arc::new(move |path: &str| routes.get(path).cloned())).await
}

/// Serve whatever `handler` returns for a path (plus `/echo`).
pub async fn serve_fn(routes: Arc<dyn Fn(&str) -> Option<Resource> + Send + Sync>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let log = Arc::new(Mutex::new(Vec::new()));
    let log2 = log.clone();
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let routes = routes.clone();
            let log = log2.clone();
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut tmp = [0u8; 8192];
                let header_end = loop {
                    let n = match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => return,
                        Ok(n) => n,
                    };
                    buf.extend_from_slice(&tmp[..n]);
                    if let Some(p) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        break p + 4;
                    }
                };
                let head = String::from_utf8_lossy(&buf[..header_end]).to_string();
                let mut lines = head.split("\r\n");
                let first = lines.next().unwrap_or("");
                let mut parts = first.split(' ');
                let method = parts.next().unwrap_or("").to_owned();
                let target = parts.next().unwrap_or("/").to_owned();
                let headers: Vec<(String, String)> = lines
                    .filter_map(|l| l.split_once(':'))
                    .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_owned()))
                    .collect();
                let len: usize = headers
                    .iter()
                    .find(|(k, _)| k == "content-length")
                    .and_then(|(_, v)| v.parse().ok())
                    .unwrap_or(0);
                let mut body = buf[header_end..].to_vec();
                while body.len() < len {
                    let n = match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => n,
                    };
                    body.extend_from_slice(&tmp[..n]);
                }
                log.lock().unwrap().push((
                    method.clone(),
                    target.clone(),
                    headers.clone(),
                    body.clone(),
                ));
                let path = target.split('?').next().unwrap_or("/").to_owned();
                let resp = if path == "/echo" {
                    let mut r = Resource::new("application/json", serde_json::json!({
                        "method": method,
                        "target": target,
                        "body": String::from_utf8_lossy(&body),
                        "ct": headers.iter().find(|(k, _)| k == "content-type").map(|(_, v)| v.clone()),
                        "x": headers.iter().find(|(k, _)| k == "x-test").map(|(_, v)| v.clone()),
                    }).to_string());
                    r.headers.push(("x-reply".into(), "yes".into()));
                    Some(r)
                } else {
                    routes(&path)
                };
                let resp = resp.unwrap_or_else(|| {
                    let mut r = Resource::new("text/plain", "not found");
                    r.status = 404;
                    r
                });
                if resp.delay_ms > 0 {
                    tokio::time::sleep(Duration::from_millis(resp.delay_ms)).await;
                }
                let mut out = format!(
                    "HTTP/1.1 {} X\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n",
                    resp.status,
                    resp.content_type,
                    resp.body.len()
                );
                for (k, v) in &resp.headers {
                    out.push_str(&format!("{k}: {v}\r\n"));
                }
                out.push_str("\r\n");
                let _ = sock.write_all(out.as_bytes()).await;
                let _ = sock.write_all(&resp.body).await;
                let _ = sock.shutdown().await;
            });
        }
    });
    Server { port, log }
}

pub fn context(virtual_time: bool) -> BrowserContext {
    let persona = Arc::new(Persona::default_chrome());
    let path = format!(
        "{}/../../profiles/tls/{}.json",
        env!("CARGO_MANIFEST_DIR"),
        persona.tls_profile_id
    );
    let tls = Arc::new(ClientHelloProfile::load(std::path::Path::new(&path)).unwrap());
    BrowserContext::new(ContextOptions {
        persona,
        profile: None,
        tls_profile: tls,
        accept_invalid_certs: false,
        virtual_time,
    })
    .unwrap()
}

pub fn browser() -> Browser {
    Browser::new(BrowserOptions {
        workers: 1,
        max_heap_bytes: Some(256 << 20),
    })
    .unwrap()
}

/// Open `url`, wait for `until`, return the page.
pub async fn open(b: &Browser, ctx: &BrowserContext, url: &str, until: WaitUntil) -> Page {
    let page = b.new_page(ctx).await.unwrap();
    page.goto(url, until, Duration::from_secs(20))
        .await
        .unwrap();
    page
}

/// `document.body.innerHTML` after the page loaded.
pub async fn body(page: &Page) -> String {
    match page.evaluate("document.body.innerHTML").await.unwrap() {
        lb_js::JsValue::String(s) => s,
        v => panic!("{v:?}"),
    }
}
