//! Default request headers per browser family, in the order the browser
//! sends them. Order is part of the fingerprint, so this is a `Vec`, not a map.
//!
//! Owner: M1 task 4.

use lb_persona::Persona;

/// `(name, value)` pairs in wire order. Names are lowercase for HTTP/2 and
/// are re-cased by the h1 transport.
pub type OrderedHeaders = Vec<(String, String)>;

/// Headers for a top-level document navigation. `sec-ch-ua*` only for Chrome.
pub fn navigation_headers(_persona: &Persona, _host: &str) -> OrderedHeaders {
    Vec::new()
}
