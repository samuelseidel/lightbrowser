//! Network input must not make the process allocate without bound
//! (GAP-090). Loopback servers only.
//!
//! A counting global allocator records the peak of live heap bytes; each
//! test resets it and checks that a hostile response never pushed the
//! peak far past the limit in force:
//! - `Content-Encoding` bombs (gzip, zstd and a stacked `gzip, zstd`), each
//!   over 1 GiB expanded, against `max_body`;
//! - a WebSocket frame that declares 60 MiB and stalls (the payload must be
//!   read as it arrives, not allocated from the declared length);
//! - a fragmented WebSocket message past the message cap (close 1009), a
//!   200 MiB declared frame and a `permessage-deflate` bomb under a lowered
//!   cap.
//!
//! The tests share the allocator, so they take one lock and run one at a
//! time.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use lb_net::fetch::NetContext;
use lb_net::websocket::{WsConnect, WsEvent};
use lb_net::{ClientHelloProfile, NetError, Request};
use lb_persona::Persona;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex, MutexGuard};

struct Counting;

static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);

fn grew(n: usize) {
    let now = LIVE.fetch_add(n, Ordering::Relaxed) + n;
    PEAK.fetch_max(now, Ordering::Relaxed);
}

// SAFETY: every call forwards to `System` with the caller's layout and only
// adds bookkeeping on the side.
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn alloc_zeroed(&self, l: Layout) -> *mut u8 {
        let p = unsafe { System.alloc_zeroed(l) };
        if !p.is_null() {
            grew(l.size());
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        unsafe { System.dealloc(p, l) };
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                grew(new - l.size());
            } else {
                LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

static ONE_AT_A_TIME: Mutex<()> = Mutex::const_new(());

async fn serial() -> MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().await
}

/// Start measuring: the peak becomes the current live size.
fn reset_peak() -> usize {
    let live = LIVE.load(Ordering::Relaxed);
    PEAK.store(live, Ordering::Relaxed);
    live
}

/// Bytes the peak rose above `base`.
fn peak_above(base: usize) -> usize {
    PEAK.load(Ordering::Relaxed).saturating_sub(base)
}

const MIB: usize = 1 << 20;
/// Expanded size of every bomb: 1 GiB.
const BOMB: usize = 1 << 30;

fn ctx() -> NetContext {
    let path = format!(
        "{}/../../profiles/tls/chrome_153_linux.json",
        env!("CARGO_MANIFEST_DIR")
    );
    let profile = ClientHelloProfile::from_json(&std::fs::read_to_string(path).unwrap()).unwrap();
    NetContext::new(Arc::new(Persona::default_chrome()), Arc::new(profile)).unwrap()
}

/// A valid gzip member of `BOMB` zero bytes (about 1 MiB): one sync-flushed
/// deflate run of 1 MiB of zeros, byte-aligned and not final, repeated;
/// it refers back at most 32 KiB, so every copy decodes alike.
fn gzip_bomb() -> Vec<u8> {
    use flate2::{Compress, Compression, Crc, FlushCompress};
    let zeros = vec![0u8; MIB];
    let mut c = Compress::new(Compression::best(), false);
    let mut run = Vec::with_capacity(64 * 1024);
    c.compress_vec(&zeros, &mut run, FlushCompress::Sync)
        .unwrap();
    assert_eq!(c.total_in() as usize, MIB, "one call takes all input");
    let copies = BOMB / MIB;
    let mut out = vec![0x1f, 0x8b, 8, 0, 0, 0, 0, 0, 0, 0xff];
    for _ in 0..copies {
        out.extend_from_slice(&run);
    }
    // A final, empty fixed-Huffman block.
    out.extend_from_slice(&[0x03, 0x00]);
    let mut one = Crc::new();
    one.update(&zeros);
    let mut crc = Crc::new();
    for _ in 0..copies {
        crc.combine(&one);
    }
    out.extend_from_slice(&crc.sum().to_le_bytes());
    out.extend_from_slice(&(BOMB as u32).to_le_bytes());
    out
}

/// A valid zstd frame of `BOMB` zero bytes (about 32 KiB): RLE blocks of
/// 128 KiB, 4 bytes each.
fn zstd_bomb() -> Vec<u8> {
    const BLOCK: usize = 128 * 1024;
    // Magic, frame header (no content size, window 128 KiB).
    let mut out = vec![0x28, 0xb5, 0x2f, 0xfd, 0x00, 0x38];
    let blocks = BOMB / BLOCK;
    for i in 0..blocks {
        let last = u32::from(i + 1 == blocks);
        let head = ((BLOCK as u32) << 3) | (1 << 1) | last;
        out.extend_from_slice(&head.to_le_bytes()[..3]);
        out.push(0);
    }
    out
}

/// A one-response HTTP/1.1 server with `body` under `encoding`.
async fn serve_encoded(encoding: &'static str, body: Vec<u8>) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut buf = vec![0u8; 8192];
        let mut got = Vec::new();
        while !got.windows(4).any(|w| w == b"\r\n\r\n") {
            let n = s.read(&mut buf).await.unwrap_or(0);
            if n == 0 {
                return;
            }
            got.extend_from_slice(&buf[..n]);
        }
        let head = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Encoding: {encoding}\r\n\
             Content-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        let _ = s.write_all(head.as_bytes()).await;
        let _ = s.write_all(&body).await;
        let _ = s.shutdown().await;
    });
    port
}

/// Fetch a bomb with `max_body` 4 MiB: the cap error, and the peak heap
/// stays near the cap (the old code inflated all 1 GiB first).
async fn fetch_bomb(encoding: &'static str, body: Vec<u8>) {
    const CAP: usize = 4 * MIB;
    let wire = body.len();
    assert!(wire < CAP, "{encoding}: {wire} bytes on the wire");
    let port = serve_encoded(encoding, body).await;
    let mut c = ctx();
    c.max_body = CAP;
    let base = reset_peak();
    let r = lb_net::fetch(
        &mut c,
        Request::get(format!("http://127.0.0.1:{port}/").parse().unwrap()),
    )
    .await;
    let peak = peak_above(base);
    match r {
        Err(NetError::BodyTooLarge { limit }) => assert_eq!(limit, CAP, "{encoding}"),
        other => panic!("{encoding}: {:?}", other.map(|r| r.body.len())),
    }
    assert!(
        peak < 4 * CAP,
        "{encoding} ({wire} bytes): peak heap rose {} MiB",
        peak / MIB
    );
}

#[tokio::test]
async fn gzip_bomb_stops_at_the_cap() {
    let _one = serial().await;
    fetch_bomb("gzip", gzip_bomb()).await;
}

#[tokio::test]
async fn zstd_bomb_stops_at_the_cap() {
    let _one = serial().await;
    let body = zstd_bomb();
    assert!(body.len() < 40 * 1024, "{}", body.len());
    fetch_bomb("zstd", body).await;
}

/// `gzip, zstd`: the server gzipped, then zstd-compressed. A few KiB on the
/// wire, 1 GiB after both layers.
#[tokio::test]
async fn stacked_bomb_stops_at_the_cap() {
    let _one = serial().await;
    let inner = gzip_bomb();
    let body = zstd::bulk::compress(&inner, 19).unwrap();
    drop(inner);
    assert!(body.len() < 16 * 1024, "{}", body.len());
    fetch_bomb("gzip, zstd", body).await;
}

fn accept_key(key: &str) -> String {
    let d = boring::hash::hash(
        boring::hash::MessageDigest::sha1(),
        format!("{key}258EAFA5-E914-47DA-95CA-C5AB0DC85B11").as_bytes(),
    )
    .unwrap();
    boring::base64::encode_block(&d)
}

/// A WebSocket server for one client: answers the handshake (accepting
/// `permessage-deflate` when `deflate`) and hands over the stream.
async fn ws_server(deflate: bool) -> (u16, tokio::task::JoinHandle<TcpStream>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let h = tokio::spawn(async move {
        let (mut s, _) = listener.accept().await.unwrap();
        let mut got = Vec::new();
        let mut b = [0u8; 1];
        while !got.ends_with(b"\r\n\r\n") {
            s.read_exact(&mut b).await.unwrap();
            got.push(b[0]);
        }
        let head = String::from_utf8(got).unwrap();
        let key = head
            .lines()
            .find_map(|l| l.strip_prefix("Sec-WebSocket-Key: "))
            .unwrap()
            .trim()
            .to_owned();
        let ext = if deflate {
            "Sec-WebSocket-Extensions: permessage-deflate\r\n"
        } else {
            ""
        };
        let reply = format!(
            "HTTP/1.1 101 Switching Protocols\r\nUpgrade: websocket\r\nConnection: Upgrade\r\n\
             Sec-WebSocket-Accept: {}\r\n{ext}\r\n",
            accept_key(&key)
        );
        s.write_all(reply.as_bytes()).await.unwrap();
        s
    });
    (port, h)
}

fn ws_connect(c: &mut NetContext, port: u16) -> WsPair {
    c.websocket(WsConnect {
        url: format!("ws://127.0.0.1:{port}/").parse().unwrap(),
        protocols: Vec::new(),
        origin: format!("http://127.0.0.1:{port}"),
    })
}

type WsPair = (
    tokio::sync::mpsc::UnboundedSender<lb_net::websocket::WsCommand>,
    tokio::sync::mpsc::UnboundedReceiver<WsEvent>,
);

async fn next(ev: &mut tokio::sync::mpsc::UnboundedReceiver<WsEvent>) -> WsEvent {
    tokio::time::timeout(Duration::from_secs(10), ev.recv())
        .await
        .expect("event in time")
        .expect("event")
}

/// A server frame header: `first` byte, then the length in its 64-bit form.
fn frame_head(first: u8, len: usize) -> Vec<u8> {
    let mut h = vec![first, 127];
    h.extend_from_slice(&(len as u64).to_be_bytes());
    h
}

/// The client's next close frame code, within 10 s (`None`: none came).
async fn close_code<R: AsyncReadExt + Unpin>(r: &mut R) -> Option<u16> {
    let read = async {
        loop {
            let mut h = [0u8; 2];
            r.read_exact(&mut h).await.ok()?;
            let mut len = usize::from(h[1] & 0x7f);
            if len == 126 {
                let mut b = [0u8; 2];
                r.read_exact(&mut b).await.ok()?;
                len = usize::from(u16::from_be_bytes(b));
            } else if len == 127 {
                let mut b = [0u8; 8];
                r.read_exact(&mut b).await.ok()?;
                len = u64::from_be_bytes(b) as usize;
            }
            let mut mask = [0u8; 4];
            r.read_exact(&mut mask).await.ok()?;
            let mut p = vec![0u8; len];
            r.read_exact(&mut p).await.ok()?;
            for (i, b) in p.iter_mut().enumerate() {
                *b ^= mask[i % 4];
            }
            if h[0] & 0x0f == 0x8 && p.len() >= 2 {
                return Some(u16::from_be_bytes([p[0], p[1]]));
            }
        }
    };
    tokio::time::timeout(Duration::from_secs(10), read)
        .await
        .ok()
        .flatten()
}

/// The page sees Chrome's failure: the message, then an abnormal close.
async fn expect_too_big(ev: &mut tokio::sync::mpsc::UnboundedReceiver<WsEvent>) {
    assert_eq!(
        next(ev).await,
        WsEvent::Failed("Max frame length exceeded".into())
    );
    assert!(matches!(next(ev).await, WsEvent::Closed { code: 1006, .. }));
}

/// A frame that declares 60 MiB (under the default 64 MiB cap), sends 1 KiB
/// and stalls: nothing near 60 MiB is allocated (the old reader made a
/// zeroed buffer of the declared length first).
#[tokio::test]
async fn ws_declared_length_is_not_allocated_up_front() {
    let _one = serial().await;
    let (port, srv) = ws_server(false).await;
    let mut c = ctx();
    let base = reset_peak();
    let (_tx, mut ev) = ws_connect(&mut c, port);
    assert!(matches!(next(&mut ev).await, WsEvent::Open { .. }));
    let mut s = srv.await.unwrap();
    s.write_all(&frame_head(0x82, 60 * MIB)).await.unwrap();
    s.write_all(&[7u8; 1024]).await.unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    let peak = peak_above(base);
    assert!(peak < 8 * MIB, "peak heap rose {} MiB", peak / MIB);
    assert!(
        ev.try_recv().is_err(),
        "no event while the frame is partial"
    );
}

/// A message assembled from fragments past the default cap (5 x 16 MiB
/// against 64 MiB): the client sends close 1009 and the page sees Chrome's
/// failure. The old code took the whole 80 MiB message.
#[tokio::test]
async fn ws_fragments_past_the_cap_close_1009() {
    let _one = serial().await;
    let (port, srv) = ws_server(false).await;
    let mut c = ctx();
    let (_tx, mut ev) = ws_connect(&mut c, port);
    assert!(matches!(next(&mut ev).await, WsEvent::Open { .. }));
    let (mut rd, mut wr) = srv.await.unwrap().into_split();
    let writer = tokio::spawn(async move {
        let chunk = vec![1u8; 16 * MIB];
        for i in 0..5 {
            let first = match i {
                0 => 0x02,
                4 => 0x80,
                _ => 0x00,
            };
            if wr.write_all(&frame_head(first, chunk.len())).await.is_err()
                || wr.write_all(&chunk).await.is_err()
            {
                return;
            }
        }
        // Keep the socket open until the test ends.
        tokio::time::sleep(Duration::from_secs(30)).await;
    });
    assert_eq!(close_code(&mut rd).await, Some(1009));
    expect_too_big(&mut ev).await;
    writer.abort();
}

/// With the cap lowered to 1 MiB, a frame declaring 200 MiB fails at once
/// (close 1009) without reading or allocating its payload.
#[tokio::test]
async fn ws_declared_200_mib_frame_over_a_lowered_cap() {
    let _one = serial().await;
    let (port, srv) = ws_server(false).await;
    let mut c = ctx();
    c.ws_max_message = MIB;
    let base = reset_peak();
    let (_tx, mut ev) = ws_connect(&mut c, port);
    assert!(matches!(next(&mut ev).await, WsEvent::Open { .. }));
    let (mut rd, mut wr) = srv.await.unwrap().into_split();
    wr.write_all(&frame_head(0x82, 200 * MIB)).await.unwrap();
    wr.write_all(&[7u8; 1024]).await.unwrap();
    assert_eq!(close_code(&mut rd).await, Some(1009));
    expect_too_big(&mut ev).await;
    let peak = peak_above(base);
    assert!(peak < 8 * MIB, "peak heap rose {} MiB", peak / MIB);
    drop(wr);
}

/// A `permessage-deflate` message of a few KiB that inflates to 8 MiB,
/// against a 1 MiB cap: inflating stops at the cap, close 1009.
#[tokio::test]
async fn ws_deflate_bomb_over_a_lowered_cap() {
    use flate2::{Compress, Compression, FlushCompress};
    let _one = serial().await;
    let mut comp = Compress::new(Compression::best(), false);
    let mut packed = Vec::with_capacity(64 * 1024);
    comp.compress_vec(&vec![0u8; 8 * MIB], &mut packed, FlushCompress::Sync)
        .unwrap();
    assert!(packed.ends_with(&[0, 0, 0xff, 0xff]));
    packed.truncate(packed.len() - 4);
    let (port, srv) = ws_server(true).await;
    let mut c = ctx();
    c.ws_max_message = MIB;
    let base = reset_peak();
    let (_tx, mut ev) = ws_connect(&mut c, port);
    assert!(matches!(next(&mut ev).await, WsEvent::Open { .. }));
    let (mut rd, mut wr) = srv.await.unwrap().into_split();
    // FIN, RSV1 (compressed), binary.
    wr.write_all(&frame_head(0xC2, packed.len())).await.unwrap();
    wr.write_all(&packed).await.unwrap();
    assert_eq!(close_code(&mut rd).await, Some(1009));
    expect_too_big(&mut ev).await;
    let peak = peak_above(base);
    assert!(peak < 4 * MIB, "peak heap rose {} MiB", peak / MIB);
    drop(wr);
}
