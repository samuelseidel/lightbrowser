//! The request pipeline: DNS → TCP → TLS (profile) → h1/h2 (ALPN) →
//! decode body → update cookie jar.
//!
//! Owner: M1 task 5.

use std::sync::Arc;

use bytes::Bytes;
use http::{Method, StatusCode};
use lb_persona::Persona;
use url::Url;

use crate::HttpVersion;
use crate::cookies::CookieJar;
use crate::error::NetError;
use crate::headers::OrderedHeaders;
use crate::profile::ClientHelloProfile;

/// An outgoing request. Headers are ordered.
#[derive(Debug, Clone)]
pub struct Request {
    pub method: Method,
    pub url: Url,
    pub headers: OrderedHeaders,
    pub body: Bytes,
}

impl Request {
    pub fn get(url: Url) -> Self {
        Self {
            method: Method::GET,
            url,
            headers: Vec::new(),
            body: Bytes::new(),
        }
    }
}

/// What a transport returns: status, headers in arrival order and the
/// undecoded body.
#[derive(Debug, Clone)]
pub struct RawResponse {
    pub status: StatusCode,
    pub headers: OrderedHeaders,
    pub body: Bytes,
    pub version: HttpVersion,
}

/// A decoded response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: StatusCode,
    pub headers: OrderedHeaders,
    pub body: Bytes,
    pub version: HttpVersion,
    pub final_url: Url,
}

impl Response {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
}

/// Per-context networking state: persona, TLS profile and cookies.
pub struct NetContext {
    pub persona: Arc<Persona>,
    pub profile: Arc<ClientHelloProfile>,
    pub cookies: CookieJar,
    /// Maximum decoded body size. Default 32 MiB.
    pub max_body: usize,
}

impl NetContext {
    pub fn new(persona: Arc<Persona>, profile: Arc<ClientHelloProfile>) -> Result<Self, NetError> {
        if let Some(fam) = profile.family()
            && fam != persona.family
        {
            return Err(NetError::FamilyMismatch {
                persona: persona.family.to_string(),
                profile: profile.profile_id.clone(),
            });
        }
        Ok(Self {
            persona,
            profile,
            cookies: CookieJar::new(),
            max_body: 32 << 20,
        })
    }
}

/// Perform `req` in `ctx`, following up to 10 redirects.
pub async fn fetch(_ctx: &mut NetContext, _req: Request) -> Result<Response, NetError> {
    Err(NetError::Http("fetch not implemented (M1 task 5)".into()))
}
