//! Connection limits: like Chrome, at most 6 simultaneous connections per
//! host; further requests queue instead of opening more sockets.
#![cfg(feature = "chrome")]

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use common::{browser, context};
use lb_js::JsValue;
use lb_page::WaitUntil;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// A 1x1 GIF.
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

#[tokio::test]
async fn at_most_six_connections_per_host() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let open = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let (open2, peak2) = (open.clone(), peak.clone());
    tokio::spawn(async move {
        loop {
            let Ok((mut sock, _)) = listener.accept().await else {
                return;
            };
            let (open, peak) = (open2.clone(), peak2.clone());
            tokio::spawn(async move {
                let now = open.fetch_add(1, Ordering::SeqCst) + 1;
                peak.fetch_max(now, Ordering::SeqCst);
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                while !buf.windows(4).any(|w| w == b"\r\n\r\n") {
                    match sock.read(&mut tmp).await {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&tmp[..n]),
                    }
                }
                let head = String::from_utf8_lossy(&buf).to_string();
                let path = head.split(' ').nth(1).unwrap_or("/").to_owned();
                let (ct, body): (&str, Vec<u8>) = if path == "/" {
                    let imgs: String = (0..20).map(|i| format!("<img src=/i{i}.gif>")).collect();
                    (
                        "text/html",
                        format!("<!DOCTYPE html><body>{imgs}</body>").into_bytes(),
                    )
                } else {
                    // Hold the connection open so the requests overlap.
                    tokio::time::sleep(Duration::from_millis(40)).await;
                    ("image/gif", GIF.to_vec())
                };
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                );
                let _ = sock.write_all(head.as_bytes()).await;
                let _ = sock.write_all(&body).await;
                let _ = sock.shutdown().await;
                open.fetch_sub(1, Ordering::SeqCst);
            });
        }
    });
    let (b, ctx) = (browser(), context(false));
    let page = b.new_page(&ctx).await.unwrap();
    page.goto(
        &format!("http://127.0.0.1:{port}/"),
        WaitUntil::Load,
        Duration::from_secs(20),
    )
    .await
    .unwrap();
    let loaded = page
        .evaluate("Array.prototype.filter.call(document.images, function (i) { return i.complete && i.naturalWidth == 1; }).length")
        .await
        .unwrap();
    assert!(
        matches!(loaded, JsValue::Number(n) if n == 20.0),
        "{loaded:?}"
    );
    let peak = peak.load(Ordering::SeqCst);
    assert!(peak <= 6, "{peak} simultaneous connections");
    assert!(peak >= 2, "requests did not overlap ({peak})");
}
