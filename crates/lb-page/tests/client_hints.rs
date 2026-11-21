//! Client hints in a real page (M7, WP-E): `Accept-CH` + `Critical-CH` on
//! the top-level document, hints on later navigations, same-origin frames
//! and subresources, none for a cross-origin frame or anything it loads.
//! Loopback only.
#![cfg(feature = "chrome")]

mod common;

use std::sync::{Arc, OnceLock};
use std::time::Duration;

use common::{Resource, browser, context, serve, serve_fn};
use lb_page::WaitUntil;

const ACCEPT_CH: &str = "Sec-CH-UA-Platform-Version, Sec-CH-UA-Arch, Sec-CH-UA-Bitness, \
Sec-CH-UA-Full-Version-List, Sec-CH-UA-Model, Sec-CH-UA-WoW64, Sec-CH-UA-Form-Factors";
const GIF: &[u8] = b"GIF89a\x01\x00\x01\x00\x80\x00\x00\x00\x00\x00\xff\xff\xff!\xf9\x04\x01\x00\x00\x00\x00,\x00\x00\x00\x00\x01\x00\x01\x00\x00\x02\x02D\x01\x00;";

type Log = Vec<(String, String, Vec<(String, String)>, Vec<u8>)>;

fn hints(h: &[(String, String)]) -> Vec<String> {
    h.iter()
        .map(|(n, _)| n.to_ascii_lowercase())
        .filter(|n| lb_net::UaHint::from_header(n).is_some())
        .collect()
}

fn find<'a>(log: &'a Log, target: &str) -> Vec<&'a Vec<(String, String)>> {
    log.iter()
        .filter(|(_, t, _, _)| t == target)
        .map(|(_, _, h, _)| h)
        .collect()
}

#[tokio::test]
async fn accept_ch_critical_ch_and_same_origin_delegation() {
    // B is a cross-origin frame that also loads an image from A, so each
    // server needs the other's port.
    let a_port: Arc<OnceLock<u16>> = Arc::default();
    let a_port2 = Arc::clone(&a_port);
    let b_srv = serve_fn(Arc::new(move |path: &str| match path {
        "/frame" => {
            let a = a_port2.get().copied().unwrap_or(0);
            let html = format!(
                "<!DOCTYPE html><img src=\"http://127.0.0.1:{a}/from-b.gif\"><img src=/b.gif>"
            );
            Some(Resource::html(&html).header("Accept-CH", ACCEPT_CH))
        }
        "/b.gif" => Some(Resource::new("image/gif", GIF)),
        _ => None,
    }))
    .await;
    let top = format!(
        "<!DOCTYPE html><img src=/i.gif><iframe src=/frame></iframe>\
<iframe src=\"{}\"></iframe><script>fetch('/api')</script>",
        b_srv.url("/frame")
    );
    let a = serve(vec![
        (
            "/",
            Resource::html(&top)
                .header("Accept-CH", ACCEPT_CH)
                .header("Critical-CH", "Sec-CH-UA-Platform-Version"),
        ),
        ("/next", Resource::html("<!DOCTYPE html>next")),
        ("/frame", Resource::html("<!DOCTYPE html><img src=/f.gif>")),
        ("/i.gif", Resource::new("image/gif", GIF)),
        ("/f.gif", Resource::new("image/gif", GIF)),
        ("/from-b.gif", Resource::new("image/gif", GIF)),
        ("/api", Resource::new("text/plain", "ok")),
    ])
    .await;
    a_port.set(a.port).unwrap();

    let (br, ctx) = (browser(), context(false));
    let page = br.new_page(&ctx).await.unwrap();
    page.goto(&a.url("/"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();
    // Let the fetch and the frames' images finish.
    for _ in 0..50 {
        let done = find(&a.requests(), "/from-b.gif").len() == 1
            && find(&a.requests(), "/api").len() == 1
            && find(&a.requests(), "/f.gif").len() == 1;
        if done {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    page.goto(&a.url("/next"), WaitUntil::Load, Duration::from_secs(20))
        .await
        .unwrap();

    let la = a.requests();
    let lb = b_srv.requests();
    let docs = find(&la, "/");
    assert_eq!(docs.len(), 2, "Critical-CH restarted the navigation once");
    assert!(hints(docs[0]).is_empty());
    let all7 = [
        "sec-ch-ua-arch",
        "sec-ch-ua-platform-version",
        "sec-ch-ua-model",
        "sec-ch-ua-bitness",
        "sec-ch-ua-wow64",
        "sec-ch-ua-full-version-list",
        "sec-ch-ua-form-factors",
    ];
    assert_eq!(hints(docs[1]), all7);
    let v = |h: &Vec<(String, String)>, n: &str| {
        h.iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(n))
            .map(|(_, v)| v.clone())
    };
    assert_eq!(
        v(docs[1], "sec-ch-ua-platform-version").as_deref(),
        Some("\"10.0.0\"")
    );
    for p in ["/i.gif", "/frame", "/f.gif", "/api"] {
        let r = find(&la, p);
        assert_eq!(r.len(), 1, "{p}");
        assert_eq!(hints(r[0]).len(), 7, "{p} carries the hints");
    }
    assert_eq!(hints(find(&la, "/next")[0]), all7);
    // Cross-origin frame: nothing, and nothing for what it loads, even
    // from the top-level origin; its own Accept-CH is ignored.
    for (log, p) in [(&lb, "/frame"), (&lb, "/b.gif"), (&la, "/from-b.gif")] {
        let r = find(log, p);
        assert_eq!(r.len(), 1, "{p}");
        assert!(hints(r[0]).is_empty(), "{p}: {:?}", hints(r[0]));
    }
    assert!(
        ctx.client_hints()
            .get(&url::Url::parse(&b_srv.url("/")).unwrap())
            .is_empty()
    );
}
