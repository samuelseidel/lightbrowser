//! Requests of a page as CDP sees them: `Network.*` events and `Fetch.*`
//! interception around the context's network stack.

use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::Ordering;

use lb_cdp::PageEvent;
use lb_cdp::types::{
    self, ContinueRequest, FulfillRequest, LoadingFailed, LoadingFinished, RequestPaused,
    RequestWillBeSent, ResponseReceived,
};
use lb_webapi::host::{Destination, NetRequest, NetResponse};
use tokio::sync::oneshot;

use super::agent::Agent;
use crate::page::PageState;

/// What the client decided for a paused request.
#[derive(Debug)]
pub(crate) enum Decision {
    Continue(ContinueRequest),
    Fulfill(FulfillRequest),
    /// `Fetch.failRequest` error reason (`Failed`, `Aborted`, ...).
    Fail(String),
}

/// CDP `resourceType` of a request.
fn resource_type(req: &NetRequest, fetch_api: bool) -> &'static str {
    match req.destination {
        Destination::Document | Destination::Iframe => "Document",
        Destination::Script => "Script",
        Destination::Style => "Stylesheet",
        Destination::Image => "Image",
        Destination::Empty => {
            if fetch_api {
                "Fetch"
            } else {
                "XHR"
            }
        }
    }
}

/// `net::ERR_*` text for a `Fetch.failRequest` reason.
pub(crate) fn fail_text(reason: &str) -> String {
    let mut out = String::from("net::ERR_");
    let mut prev_lower = false;
    for c in reason.chars() {
        if c.is_ascii_uppercase() && prev_lower {
            out.push('_');
        }
        prev_lower = c.is_ascii_lowercase();
        out.push(c.to_ascii_uppercase());
    }
    out
}

/// A Chrome-style `net::ERR_*` text for an lb-net error message.
pub(crate) fn net_error_text(e: &str) -> String {
    if e.starts_with("net::") {
        return e.to_string();
    }
    let l = e.to_ascii_lowercase();
    let code = if l.contains("refused") {
        "CONNECTION_REFUSED"
    } else if l.contains("reset") {
        "CONNECTION_RESET"
    } else if l.contains("timed out") || l.contains("timeout") {
        "TIMED_OUT"
    } else if l.contains("resolve") || l.contains("dns") || l.contains("lookup") {
        "NAME_NOT_RESOLVED"
    } else if l.contains("certificate") || l.contains("tls") || l.contains("ssl") {
        "CERT_AUTHORITY_INVALID"
    } else {
        "FAILED"
    };
    format!("net::ERR_{code}")
}

fn glob(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) && p[pi] != '*' {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => u32::from(c - b'A'),
            b'a'..=b'z' => u32::from(c - b'a') + 26,
            b'0'..=b'9' => u32::from(c - b'0') + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        })
    };
    let mut out = Vec::with_capacity(s.len() * 3 / 4);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in s.bytes().filter(|c| !c.is_ascii_whitespace() && *c != b'=') {
        acc = (acc << 6) | val(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

pub(crate) fn base64_encode(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (u32::from(b[0]) << 16) | (u32::from(b[1]) << 8) | u32::from(b[2]);
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            T[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            T[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn cdp_request(agent: &Agent, page: &PageState, req: &NetRequest) -> types::NetRequest {
    let persona = page.ctx.persona();
    let mut headers = BTreeMap::new();
    headers.insert("User-Agent".to_string(), persona.user_agent.clone());
    headers.insert(
        "Accept-Language".to_string(),
        persona.accept_language.clone(),
    );
    if let Some(ua) = &persona.sec_ch_ua {
        headers.insert("sec-ch-ua".to_string(), ua.clone());
    }
    for (k, v) in &req.headers {
        headers.insert(k.clone(), v.clone());
    }
    if let Some(i) = &req.initiator {
        headers.insert("Referer".to_string(), i.to_string());
    }
    let _ = agent;
    let body = (!req.body.is_empty()).then(|| String::from_utf8_lossy(&req.body).into_owned());
    types::NetRequest {
        url: req.url.to_string(),
        method: req.method.clone(),
        headers,
        has_post_data: body.as_ref().map(|_| true),
        post_data: body,
        initial_priority: match req.destination {
            Destination::Document | Destination::Iframe | Destination::Style => "VeryHigh",
            Destination::Script => "High",
            Destination::Image => "Low",
            Destination::Empty => "High",
        }
        .into(),
        referrer_policy: "strict-origin-when-cross-origin".into(),
    }
}

fn cdp_response(resp: &NetResponse) -> types::NetResponse {
    let mut headers = BTreeMap::new();
    for (k, v) in &resp.headers {
        headers
            .entry(k.clone())
            .and_modify(|e: &mut String| {
                e.push('\n');
                e.push_str(v);
            })
            .or_insert_with(|| v.clone());
    }
    let ct = resp.header("content-type").unwrap_or("");
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let charset = ct
        .split(';')
        .skip(1)
        .find_map(|p| p.trim().strip_prefix("charset="))
        .unwrap_or("")
        .trim_matches('"')
        .to_ascii_lowercase();
    types::NetResponse {
        url: resp.url.to_string(),
        status: i64::from(resp.status),
        status_text: resp.status_text.clone(),
        headers,
        mime_type: mime,
        charset,
        encoded_data_length: resp.body.len() as i64,
        remote_ip_address: resp
            .url
            .host_str()
            .map(|h| h.trim_matches(['[', ']']).to_string()),
        remote_port: resp.url.port_or_known_default().map(i64::from),
        protocol: Some("http/1.1".into()),
        security_state: if resp.url.scheme() == "https" {
            "secure"
        } else {
            "insecure"
        }
        .into(),
        ..Default::default()
    }
}

/// A request of `page` through its context's network stack, with CDP
/// events and interception when the page has an agent. `request_id` is
/// given for navigations (the loader id).
pub(crate) async fn page_fetch(
    page: &Rc<PageState>,
    mut req: NetRequest,
    request_id: Option<String>,
) -> Result<NetResponse, String> {
    let Some(agent) = page.agent() else {
        return page.ctx.fetch(req).await;
    };
    let (extra, intercept) = {
        let st = agent.st.borrow();
        (st.extra_headers.clone(), st.intercept.clone())
    };
    for (k, v) in extra {
        if !req.headers.iter().any(|(n, _)| n.eq_ignore_ascii_case(&k)) {
            req.headers.push((k, v));
        }
    }
    let fetch_api = req.mode != "navigate" && req.destination == Destination::Empty;
    let rtype = resource_type(&req, fetch_api);
    let id = request_id.clone().unwrap_or_else(|| {
        format!(
            "{}.{}",
            agent.page_id,
            agent.shared.next_request.fetch_add(1, Ordering::Relaxed)
        )
    });
    let loader = agent.st.borrow().loader.clone();
    let loader = request_id.clone().unwrap_or(loader);
    let frame = agent.target.clone();
    let doc_url = page
        .window()
        .map(|w| w.url().to_string())
        .unwrap_or_else(|| req.url.to_string());
    agent.emit(PageEvent::RequestWillBeSent(Box::new(RequestWillBeSent {
        request_id: id.clone(),
        loader_id: loader.clone(),
        document_url: if rtype == "Document" {
            req.url.to_string()
        } else {
            doc_url
        },
        request: cdp_request(&agent, page, &req),
        timestamp: agent.mono(),
        wall_time: wall_secs(),
        initiator: if req.initiator.is_some() && rtype != "Document" {
            serde_json::json!({ "type": "script" })
        } else {
            serde_json::json!({ "type": "other" })
        },
        redirect_has_extra_info: false,
        redirect_response: None,
        resource_type: rtype.into(),
        frame_id: Some(frame.clone()),
        has_user_gesture: false,
    })));

    // Interception.
    let paused = intercept.as_ref().is_some_and(|pats| {
        pats.iter().any(|p| {
            glob(p.url_pattern.as_deref().unwrap_or("*"), req.url.as_str())
                && p.resource_type.as_deref().is_none_or(|t| t == rtype)
                && p.request_stage.as_deref() != Some("Response")
        })
    });
    let mut fulfilled: Option<NetResponse> = None;
    if paused {
        let iid = format!(
            "interception-job-{}.0",
            agent.shared.next_intercept.fetch_add(1, Ordering::Relaxed)
        );
        let (tx, rx) = oneshot::channel();
        agent
            .shared
            .paused
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(iid.clone(), (agent.target.clone(), tx));
        agent.emit(PageEvent::RequestPaused(Box::new(RequestPaused {
            request_id: iid,
            request: cdp_request(&agent, page, &req),
            frame_id: frame.clone(),
            resource_type: rtype.into(),
            network_id: Some(id.clone()),
            response_status_code: None,
        })));
        match rx.await {
            Err(_) => return Err("net::ERR_ABORTED".into()),
            Ok(Decision::Fail(reason)) => {
                let text = fail_text(&reason);
                agent.emit(PageEvent::LoadingFailed(LoadingFailed {
                    request_id: id,
                    timestamp: agent.mono(),
                    resource_type: rtype.into(),
                    error_text: text.clone(),
                    canceled: false,
                }));
                return Err(text);
            }
            Ok(Decision::Continue(c)) => {
                if let Some(u) = c.url.as_deref().and_then(|u| url::Url::parse(u).ok()) {
                    req.url = u;
                }
                if let Some(m) = c.method {
                    req.method = m;
                }
                if let Some(h) = c.headers {
                    req.headers = h.into_iter().map(|e| (e.name, e.value)).collect();
                }
                if let Some(b) = c.post_data {
                    req.body = base64_decode(&b).unwrap_or_else(|| b.into_bytes());
                }
            }
            Ok(Decision::Fulfill(f)) => {
                let body = f
                    .body
                    .as_deref()
                    .and_then(base64_decode)
                    .unwrap_or_default();
                let headers: Vec<(String, String)> = f
                    .response_headers
                    .unwrap_or_default()
                    .into_iter()
                    .map(|h| (h.name, h.value))
                    .collect();
                let status = u16::try_from(f.response_code).unwrap_or(200);
                fulfilled = Some(NetResponse {
                    status,
                    status_text: f.response_phrase.unwrap_or_else(|| {
                        http::StatusCode::from_u16(status)
                            .ok()
                            .and_then(|s| s.canonical_reason())
                            .unwrap_or("")
                            .to_string()
                    }),
                    headers,
                    body,
                    url: req.url.clone(),
                    redirected: false,
                });
            }
        }
    }
    let r = match fulfilled {
        Some(r) => Ok(r),
        None => page.ctx.fetch(req).await,
    };
    match &r {
        Ok(resp) => {
            agent.emit(PageEvent::ResponseReceived(Box::new(ResponseReceived {
                request_id: id.clone(),
                loader_id: loader,
                timestamp: agent.mono(),
                resource_type: rtype.into(),
                response: cdp_response(resp),
                has_extra_info: false,
                frame_id: Some(frame),
            })));
            agent.store_body(&id, resp.body.clone());
            let len = resp.body.len() as f64;
            // Navigations finish loading once the document is parsed; the
            // event follows the commit like in Chrome.
            agent.emit(PageEvent::LoadingFinished(LoadingFinished {
                request_id: id,
                timestamp: agent.mono(),
                encoded_data_length: len,
            }));
        }
        Err(e) => {
            agent.emit(PageEvent::LoadingFailed(LoadingFailed {
                request_id: id,
                timestamp: agent.mono(),
                resource_type: rtype.into(),
                error_text: net_error_text(e),
                canceled: false,
            }));
        }
    }
    r.map_err(|e| net_error_text(&e))
}

fn wall_secs() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helpers() {
        assert!(glob("*", "http://a/b"));
        assert!(glob(
            "http://127.0.0.1:*/api/*",
            "http://127.0.0.1:80/api/x"
        ));
        assert!(!glob("*.png", "http://a/b.css"));
        assert_eq!(fail_text("Failed"), "net::ERR_FAILED");
        assert_eq!(
            fail_text("ConnectionRefused"),
            "net::ERR_CONNECTION_REFUSED"
        );
        assert_eq!(fail_text("BlockedByClient"), "net::ERR_BLOCKED_BY_CLIENT");
        assert_eq!(
            base64_decode(&base64_encode(b"hello!?")).unwrap(),
            b"hello!?"
        );
        assert_eq!(base64_encode(b"ab"), "YWI=");
        assert_eq!(
            net_error_text("connect: Connection refused (os error 111)"),
            "net::ERR_CONNECTION_REFUSED"
        );
    }
}
