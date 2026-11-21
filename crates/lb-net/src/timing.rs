//! What a fetch measured, for Resource Timing and Navigation Timing
//! (ADR 0046). All values are [`Instant`]s of the network layer; the page
//! layer turns them into milliseconds on a page clock.

use std::time::Instant;

/// Setup of one connection: DNS, TCP connect and, for https, the TLS
/// handshake. An IP literal has no DNS step (`dns_start == dns_end`).
#[derive(Debug, Clone, Copy)]
pub struct ConnTiming {
    pub dns_start: Instant,
    pub dns_end: Instant,
    /// TCP connect start (equal to `dns_end`).
    pub connect_start: Instant,
    /// TLS handshake start (https only).
    pub secure_start: Option<Instant>,
    /// TCP connect end, or the end of the TLS handshake for https (the
    /// value Resource Timing calls `connectEnd`).
    pub connect_end: Instant,
}

impl ConnTiming {
    /// A connection set up between `start` and `end` with no steps told
    /// apart (a proxy tunnel).
    pub fn whole(start: Instant, end: Instant) -> ConnTiming {
        ConnTiming {
            dns_start: start,
            dns_end: start,
            connect_start: start,
            secure_start: None,
            connect_end: end,
        }
    }
}

/// One redirect that was followed (for the Timing-Allow-Origin check).
#[derive(Debug, Clone)]
pub struct RedirectTiming {
    pub url: url::Url,
    /// Values of the response's `Timing-Allow-Origin` headers.
    pub timing_allow_origin: Vec<String>,
}

/// The measurements of one [`crate::fetch`] call.
#[derive(Debug, Clone)]
pub struct FetchTiming {
    /// The fetch began (first hop).
    pub start: Instant,
    /// With redirects: when the first hop began and when the last redirect
    /// response was complete.
    pub redirect: Option<(Instant, Instant)>,
    pub redirects: Vec<RedirectTiming>,
    /// The final hop began.
    pub fetch_start: Instant,
    /// Setup of the connection that carried the final request when it was
    /// opened for it; `None` for a reused connection or session.
    pub conn: Option<ConnTiming>,
    /// The request was about to be written.
    pub request_start: Instant,
    /// The response head had arrived.
    pub response_start: Instant,
    /// The body had arrived.
    pub response_end: Instant,
    /// Body bytes as received (before `Content-Encoding` is undone).
    pub encoded_body_size: u64,
    pub decoded_body_size: u64,
}
