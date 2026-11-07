//! Time zone, locale, `acceptLanguage` and geolocation overrides
//! (ADR 0039): Chrome's rules for who may set and clear them.
//!
//! In Chrome each page's DevTools agent keeps its own overrides; the time
//! zone and the ICU locale belong to the renderer process
//! (`TimeZoneController`, `LocaleController`), so a second session of the
//! same page gets "Timezone override is already in effect" / "Another
//! locale override is already in effect", clearing only works for the
//! session that set it, and an override ends when that session detaches.
//! A zone id an override already in effect is changed to is not checked
//! (Chrome ignores an unknown one silently); a first one is ("Invalid
//! timezone id").
//!
//! LightBrowser has one ICU for the whole process, so the time zone and
//! locale apply to a browser context (all its pages and workers), as if
//! the context were one renderer process. Pages of a context may all set
//! the same value (Playwright sets `timezoneId` on each page of a
//! context); each holds it until its session detaches. A page asking for a
//! different value than another page of its context holds gets Chrome's
//! "already in effect" error. `acceptLanguage` and geolocation are per
//! page, as in Chrome.
//!
//! All expectations are recorded from Chrome 153 (`tests/emulation/`).

use std::collections::HashMap;

use serde_json::Value;

use super::{Call, host_err};
use crate::emulation;
use crate::host::PageHost;
use crate::protocol::CdpError;
use crate::types::{BrowserContextId, TargetId};

/// Overrides of every page, by target id.
#[derive(Debug, Default)]
pub(crate) struct Overrides {
    pages: HashMap<TargetId, PageOverrides>,
}

/// One page's overrides: (session key, value) for the time zone and the
/// locale, the session for `acceptLanguage` and geolocation.
#[derive(Debug, Default)]
struct PageOverrides {
    context: BrowserContextId,
    time_zone: Option<(String, String)>,
    locale: Option<(String, String)>,
    accept_language: Option<String>,
    geolocation: Option<String>,
}

impl PageOverrides {
    fn is_empty(&self) -> bool {
        self.time_zone.is_none()
            && self.locale.is_none()
            && self.accept_language.is_none()
            && self.geolocation.is_none()
    }
}

/// Which of the two context-wide values.
#[derive(Clone, Copy, PartialEq)]
enum Wide {
    TimeZone,
    Locale,
}

impl Overrides {
    fn entry(&mut self, target: &str, context: &str) -> &mut PageOverrides {
        let e = self.pages.entry(target.to_owned()).or_default();
        e.context = context.to_owned();
        e
    }

    fn slot(p: &PageOverrides, w: Wide) -> &Option<(String, String)> {
        match w {
            Wide::TimeZone => &p.time_zone,
            Wide::Locale => &p.locale,
        }
    }

    fn slot_mut(p: &mut PageOverrides, w: Wide) -> &mut Option<(String, String)> {
        match w {
            Wide::TimeZone => &mut p.time_zone,
            Wide::Locale => &mut p.locale,
        }
    }

    /// The value in effect in `context`, from pages other than `except`.
    fn held_elsewhere(&self, context: &str, except: &str, w: Wide) -> Option<String> {
        self.pages
            .iter()
            .filter(|(t, p)| t.as_str() != except && p.context == context)
            .find_map(|(_, p)| Self::slot(p, w).as_ref().map(|(_, v)| v.clone()))
    }

    fn page_slot(&self, target: &str, w: Wide) -> Option<(String, String)> {
        self.pages
            .get(target)
            .and_then(|p| Self::slot(p, w).clone())
    }

    fn set_slot(&mut self, target: &str, context: &str, w: Wide, v: Option<(String, String)>) {
        *Self::slot_mut(self.entry(target, context), w) = v;
        self.prune(target);
    }

    fn prune(&mut self, target: &str) {
        if self.pages.get(target).is_some_and(PageOverrides::is_empty) {
            self.pages.remove(target);
        }
    }
}

/// What the host must be told after a session or page went away.
#[derive(Debug, Default)]
pub(crate) struct Released {
    /// Contexts whose time zone / locale override ended.
    time_zone: Vec<BrowserContextId>,
    locale: Vec<BrowserContextId>,
    /// Pages whose `acceptLanguage` / geolocation override ended.
    accept_language: Vec<TargetId>,
    geolocation: Vec<TargetId>,
}

impl Overrides {
    /// Drop what `session` set (it detached: Chrome's agent is gone).
    pub fn release_session(&mut self, session: &str) -> Released {
        self.release(|_, owner| owner == session)
    }

    /// Drop the overrides of page `target` (it closed).
    pub fn release_target(&mut self, target: &str) -> Released {
        self.release(|t, _| t == target)
    }

    fn release(&mut self, gone: impl Fn(&str, &str) -> bool) -> Released {
        let mut r = Released::default();
        let targets: Vec<TargetId> = self.pages.keys().cloned().collect();
        for t in &targets {
            let Some(p) = self.pages.get_mut(t) else {
                continue;
            };
            let ctx = p.context.clone();
            if p.time_zone.as_ref().is_some_and(|(s, _)| gone(t, s)) {
                p.time_zone = None;
                r.time_zone.push(ctx.clone());
            }
            if p.locale.as_ref().is_some_and(|(s, _)| gone(t, s)) {
                p.locale = None;
                r.locale.push(ctx.clone());
            }
            if p.accept_language.as_deref().is_some_and(|s| gone(t, s)) {
                p.accept_language = None;
                r.accept_language.push(t.clone());
            }
            if p.geolocation.as_deref().is_some_and(|s| gone(t, s)) {
                p.geolocation = None;
                r.geolocation.push(t.clone());
            }
        }
        for t in &targets {
            self.prune(t);
        }
        // A context value ends only when no page holds it any more.
        r.time_zone.sort();
        r.time_zone.dedup();
        r.locale.sort();
        r.locale.dedup();
        r.time_zone
            .retain(|c| self.held_elsewhere(c, "", Wide::TimeZone).is_none());
        r.locale
            .retain(|c| self.held_elsewhere(c, "", Wide::Locale).is_none());
        r
    }
}

impl Released {
    /// Tell the host (errors ignored: the page may be gone).
    pub fn apply<H: PageHost>(self, host: &H) {
        for c in self.time_zone {
            let _ = host.set_time_zone_override(&c, "", None);
        }
        for c in self.locale {
            let _ = host.set_locale_override(&c, "", None);
        }
        for t in self.accept_language {
            let _ = host.set_accept_language_override(&t, None);
        }
        for t in self.geolocation {
            let _ = host.set_geolocation_override(&t, None);
        }
    }
}

fn already(w: Wide) -> CdpError {
    CdpError::server(match w {
        Wide::TimeZone => "Timezone override is already in effect",
        Wide::Locale => "Another locale override is already in effect",
    })
}

/// `Emulation.setTimezoneOverride` (Blink's `InspectorEmulationAgent`).
pub(crate) fn set_time_zone<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<(), CdpError> {
    let zone = emulation::req_string(c.p, "timezoneId")?.to_owned();
    let t = c.page()?;
    let ctx = c.context_of_page()?;
    let me = c.key.clone();
    let w = Wide::TimeZone;
    let (page, elsewhere) = {
        let st = c.sh.state.borrow();
        (
            st.overrides.page_slot(&t, w),
            st.overrides.held_elsewhere(&ctx, &t, w),
        )
    };
    let current = page
        .as_ref()
        .map(|(_, z)| z.clone())
        .or_else(|| elsewhere.clone())
        .unwrap_or_default();
    if zone == current {
        // Chrome: the id in effect is a no-op. A page of the context that
        // did not hold it yet holds it now (its own renderer in Chrome).
        if !zone.is_empty() && page.is_none() {
            c.sh.state
                .borrow_mut()
                .overrides
                .set_slot(&t, &ctx, w, Some((me, zone)));
        }
        return Ok(());
    }
    let mine = page.as_ref().is_some_and(|(s, _)| *s == me);
    if zone.is_empty() {
        if mine {
            c.sh.state
                .borrow_mut()
                .overrides
                .set_slot(&t, &ctx, w, None);
            if elsewhere.is_none() {
                c.sh.host
                    .set_time_zone_override(&ctx, &t, None)
                    .map_err(host_err)?;
            }
        }
        return Ok(());
    }
    if (page.is_some() && !mine) || elsewhere.is_some() {
        return Err(already(w));
    }
    let known =
        c.sh.host
            .set_time_zone_override(&ctx, &t, Some(&zone))
            .map_err(host_err)?;
    if !known {
        // A change of this session's own override ignores an unknown id.
        return if mine {
            Ok(())
        } else {
            Err(CdpError::new(
                crate::protocol::code::INVALID_PARAMS,
                "Invalid timezone id",
            ))
        };
    }
    c.sh.state
        .borrow_mut()
        .overrides
        .set_slot(&t, &ctx, w, Some((me, zone)));
    Ok(())
}

/// `Emulation.setLocaleOverride` (Blink's `InspectorEmulationAgent` and
/// `LocaleController`).
pub(crate) fn set_locale<H: PageHost + 'static>(c: &Call<'_, H>) -> Result<(), CdpError> {
    let locale = emulation::opt_string(c.p, "locale")?
        .unwrap_or_default()
        .to_owned();
    let t = c.page()?;
    let ctx = c.context_of_page()?;
    let me = c.key.clone();
    let w = Wide::Locale;
    let (page, elsewhere) = {
        let st = c.sh.state.borrow();
        (
            st.overrides.page_slot(&t, w),
            st.overrides.held_elsewhere(&ctx, &t, w),
        )
    };
    let mine = page.as_ref().is_some_and(|(s, _)| *s == me);
    // Another session's override of this page: any call fails.
    if page.is_some() && !mine {
        return Err(already(w));
    }
    let current = page
        .as_ref()
        .map(|(_, l)| l.clone())
        .or_else(|| elsewhere.clone())
        .unwrap_or_default();
    if locale == current {
        if !locale.is_empty() && page.is_none() {
            c.sh.state
                .borrow_mut()
                .overrides
                .set_slot(&t, &ctx, w, Some((me, locale)));
        }
        return Ok(());
    }
    if locale.is_empty() {
        if mine {
            c.sh.state
                .borrow_mut()
                .overrides
                .set_slot(&t, &ctx, w, None);
            if elsewhere.is_none() {
                c.sh.host
                    .set_locale_override(&ctx, &t, None)
                    .map_err(host_err)?;
            }
        }
        return Ok(());
    }
    if !emulation::locale_is_valid(&locale) {
        return Err(CdpError::server("Invalid locale name"));
    }
    if elsewhere.is_some() {
        return Err(already(w));
    }
    let tag = emulation::icu_locale_tag(&locale);
    c.sh.host
        .set_locale_override(&ctx, &t, Some(&tag))
        .map_err(host_err)?;
    c.sh.state
        .borrow_mut()
        .overrides
        .set_slot(&t, &ctx, w, Some((me, locale)));
    Ok(())
}

/// `acceptLanguage` of `Emulation/Network.setUserAgentOverride`: set, or
/// cleared when absent or empty (Blink stores `value_or("")`).
pub(crate) fn set_accept_language<H: PageHost + 'static>(
    c: &Call<'_, H>,
    t: &str,
) -> Result<(), CdpError> {
    let al =
        c.p.get("acceptLanguage")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
    // A worker target may have no context: the override is per target.
    let ctx = c.context_of_page().unwrap_or_default();
    let had =
        c.sh.state
            .borrow()
            .overrides
            .pages
            .get(t)
            .is_some_and(|p| p.accept_language.is_some());
    if al.is_none() && !had {
        return Ok(());
    }
    c.sh.host
        .set_accept_language_override(t, al)
        .map_err(host_err)?;
    let mut st = c.sh.state.borrow_mut();
    st.overrides.entry(t, &ctx).accept_language = al.map(|_| c.key.clone());
    st.overrides.prune(t);
    Ok(())
}

/// `Emulation.setGeolocationOverride` / `clearGeolocationOverride`.
pub(crate) fn set_geolocation<H: PageHost + 'static>(
    c: &Call<'_, H>,
    clear: bool,
) -> Result<(), CdpError> {
    let t = c.page()?;
    let ctx = c.context_of_page()?;
    let geo = if clear {
        None
    } else {
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0.0, |d| d.as_millis() as f64);
        Some(emulation::geolocation(c.p, now)?)
    };
    let set = geo.is_some();
    c.sh.host
        .set_geolocation_override(&t, geo)
        .map_err(host_err)?;
    let mut st = c.sh.state.borrow_mut();
    st.overrides.entry(&t, &ctx).geolocation = set.then(|| c.key.clone());
    st.overrides.prune(&t);
    Ok(())
}
