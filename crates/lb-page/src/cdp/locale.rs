//! CDP time zone, locale, `acceptLanguage` and geolocation overrides on
//! real pages (ADR 0039). The server keeps Chrome's rules for who may set
//! them (`lb_cdp` `browser/overrides.rs`); this applies what it decided.
//!
//! The time zone and ICU locale belong to the browser context (ICU's
//! defaults are process-wide, as they are per renderer process in
//! Chrome): the context's effective persona changes, so new realms
//! (documents, frames, workers, service workers) and `fingerprint-report`
//! follow it, and every isolate that already runs one of the context's
//! realms is told (the page threads, and each worker through its inbox),
//! so `Date` and `Intl` change at once, as in Chrome. `acceptLanguage` and
//! geolocation belong to the page (its agent).

use lb_cdp::{GeolocationOverride, HostError, HostResult};
use lb_webapi::host::{GeoAnswer, GeoCoords};

use super::CdpHost;
use super::agent::Agent;

impl Agent {
    /// The page's `navigator.languages` under an `acceptLanguage`
    /// override.
    pub(crate) fn languages_override(&self) -> Option<Vec<String>> {
        let st = self.st.borrow();
        st.accept_language
            .as_deref()
            .map(lb_cdp::emulation::navigator_languages)
    }

    /// The `Accept-Language` of the page's requests under an override.
    pub(crate) fn accept_language_header(&self) -> Option<String> {
        let st = self.st.borrow();
        st.accept_language
            .as_deref()
            .map(lb_cdp::emulation::accept_language_header)
    }

    /// What `navigator.geolocation` answers.
    pub(crate) fn geolocation(&self) -> Option<GeoAnswer> {
        Some(match self.st.borrow().geolocation.as_ref()? {
            GeolocationOverride::Unavailable => GeoAnswer::Unavailable,
            GeolocationOverride::Position(p) => GeoAnswer::Position(GeoCoords {
                latitude: p.latitude,
                longitude: p.longitude,
                accuracy: p.accuracy,
                altitude: p.altitude,
                altitude_accuracy: p.altitude_accuracy,
                heading: p.heading,
                speed: p.speed,
                timestamp: p.timestamp,
            }),
        })
    }
}

impl CdpHost {
    /// `Emulation.setTimezoneOverride` for `context`.
    pub(super) fn time_zone_override(
        &self,
        context: &str,
        target: &str,
        zone: Option<&str>,
    ) -> HostResult<bool> {
        let ctx = self.context(context)?;
        if let Some(z) = zone {
            // ICU decides, as Blink's `TimeZoneController` (on the page's
            // thread when there is one: its isolate is told at once).
            let z = z.to_owned();
            let known = if self.pages.borrow().contains_key(target) {
                self.blocking(target, move |ws, _, _| {
                    Ok(ws.with_engine(|e| e.set_time_zone(&z)).unwrap_or(false))
                })?
            } else {
                lb_js::set_process_time_zone(&z)
            };
            if !known {
                return Ok(false);
            }
        }
        ctx.set_time_zone_override(zone);
        self.apply_intl(context, &ctx);
        Ok(true)
    }

    /// `Emulation.setLocaleOverride` for `context`.
    pub(super) fn locale_override(&self, context: &str, locale: Option<&str>) -> HostResult<()> {
        let ctx = self.context(context)?;
        ctx.set_locale_override(locale);
        self.apply_intl(context, &ctx);
        Ok(())
    }

    /// Tell every isolate running a realm of `context` its zone and
    /// locale (read when each job runs: see `BrowserContext::with_intl`).
    fn apply_intl(&self, context: &str, ctx: &crate::BrowserContext) {
        let targets: Vec<String> = self
            .pages
            .borrow()
            .iter()
            .filter(|(_, p)| p.context == context)
            .map(|(t, _)| t.clone())
            .collect();
        for t in targets {
            let _ = self.post(&t, |ws, p, _| {
                p.ctx.with_intl(|zone, locale| {
                    ws.with_engine(|e| {
                        e.set_default_locale(locale);
                        e.set_time_zone(zone);
                    })
                });
                if let Some(w) = p.window() {
                    lb_webapi::rt::emulation::notify_workers(&w);
                }
            });
        }
        let mut inboxes: Vec<lb_webapi::rt::Inbox> = ctx
            .shared_workers()
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|(i, _)| i.clone())
            .collect();
        inboxes.extend(ctx.service_workers().inboxes());
        for i in inboxes {
            i.send(lb_webapi::rt::emulation::intl_task());
        }
    }

    /// `acceptLanguage` of a page.
    pub(super) fn accept_language_override(
        &self,
        target: &str,
        languages: Option<&str>,
    ) -> HostResult<()> {
        let languages = languages.map(str::to_owned);
        if let Some(w) = self.worker(target) {
            return w.send(super::workers::WorkerCmd::AcceptLanguage(languages));
        }
        self.post(target, move |_, p, a| {
            a.st.borrow_mut().accept_language = languages;
            if let Some(w) = p.window() {
                lb_webapi::rt::emulation::languages_changed(&w);
                for d in lb_webapi::rt::frames::descendants(&w) {
                    lb_webapi::rt::emulation::languages_changed(&d);
                }
            }
        })
    }

    /// Geolocation of a page.
    pub(super) fn geolocation_override(
        &self,
        target: &str,
        geo: Option<GeolocationOverride>,
    ) -> HostResult<()> {
        if !self.pages.borrow().contains_key(target) {
            return Err(HostError::new("No target with given id found"));
        }
        let set = geo.is_some();
        self.post(target, move |_, p, a| {
            a.st.borrow_mut().geolocation = geo;
            if let Some(w) = p.window() {
                lb_webapi::rt::emulation::geolocation_changed(&w, set);
                for d in lb_webapi::rt::frames::descendants(&w) {
                    lb_webapi::rt::emulation::geolocation_changed(&d, set);
                }
            }
        })
    }
}
