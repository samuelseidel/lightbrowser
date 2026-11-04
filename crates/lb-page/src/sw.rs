//! Service worker registry of a browser context (ADR 0022), in memory.
//!
//! Registrations, versions (one worker thread each, `worker.rs`), the
//! listed clients and the Cache API storage of one context. Windows reach
//! it through [`SwHostImpl`] (`lb_webapi::rt::sw::SwHost`); it reaches
//! windows and service worker scopes through their inboxes with the tasks
//! `lb_webapi::rt::sw` builds, in the order the Service Workers algorithms
//! (and Chrome) produce them:
//!
//! - **Jobs** (`register`, `update`, soft updates) run one at a time per
//!   context on the calling thread's local set: fetch the script (status,
//!   JavaScript MIME type, maximum scope / `Service-Worker-Allowed`),
//!   compare it with the newest version, start a version and run its
//!   script, then Install: `installing`, the job's promise, `updatefound`,
//!   the `install` event, `waiting` / `installed`.
//! - **Activation** (outside the job queue): `active`, `activating`, the
//!   controllers of the old version's clients, `ready`, the `activate`
//!   event, `activated`; it waits while the active version controls
//!   clients unless `skipWaiting()` was called.
//! - **Fetch**: a controlled client's request waits until its version is
//!   activated, then becomes a `fetch` event (no fetch handler: network).
//! - Nothing touches disk; versions end with their context.

use std::collections::HashMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use lb_webapi::host::{Destination, LocalFuture, NetRequest};
use lb_webapi::rt::sw::{
    self as s, CacheStorageRef, ClientInfo, ClientReg, FetchOutcome, RegInfo, Reply, ServiceStart,
    SwError, SwHost, SwRequest, SwState, VersionInfo,
};
use lb_webapi::rt::{Inbox, RemoteTask, WorkerControl, WorkerKind, WorkerStart};
use tokio::sync::oneshot;
use url::Url;

use crate::context::{BrowserContext, WeakContext};

/// Delay of the update check after a controlled navigation (Chrome on
/// loopback: 1.5-2 s after the navigation).
const SOFT_UPDATE_DELAY: Duration = Duration::from_millis(1500);

/// A running version with no event in flight is stopped this long after
/// its last event, and started again by the next one (Chrome 153 on
/// loopback: exactly 30 s; never while DevTools is attached to it).
pub(crate) const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

impl SwShared {
    /// The idle timeout of this context's versions.
    pub(crate) fn idle_timeout(&self) -> Duration {
        match self.idle_ms.load(std::sync::atomic::Ordering::Relaxed) {
            0 => IDLE_TIMEOUT,
            ms => Duration::from_millis(ms),
        }
    }

    pub(crate) fn set_idle_timeout(&self, d: Duration) {
        self.idle_ms.store(
            d.as_millis().max(1) as u64,
            std::sync::atomic::Ordering::Relaxed,
        );
    }
}

/// The registry of one context.
#[derive(Default)]
pub(crate) struct SwShared {
    st: Mutex<Registry>,
    /// Held while a stopped version is started again (one thread each).
    restart: Mutex<()>,
    /// [`IDLE_TIMEOUT`] unless a test shortened it (milliseconds; 0 =
    /// the default).
    idle_ms: std::sync::atomic::AtomicU64,
    /// The job queue (one job at a time).
    jobs: tokio::sync::Mutex<()>,
    /// Version states changed (fetches wait for `activated`).
    changed: tokio::sync::Notify,
}

#[derive(Default)]
struct Registry {
    regs: Vec<Reg>,
    versions: HashMap<u64, Version>,
    clients: Vec<Client>,
    next: u64,
    ready: Vec<(Url, Reply<RegInfo>)>,
    caches: HashMap<String, CacheStorageRef>,
}

struct Reg {
    id: u64,
    scope: Url,
    update_via_cache: String,
    installing: Option<u64>,
    waiting: Option<u64>,
    active: Option<u64>,
    uninstalling: bool,
}

impl Reg {
    fn newest(&self) -> Option<u64> {
        self.installing.or(self.waiting).or(self.active)
    }

    fn origin(&self) -> String {
        self.scope.origin().ascii_serialization()
    }
}

struct Version {
    reg: u64,
    script_url: Url,
    state: SwState,
    source: String,
    has_fetch: bool,
    inbox: Option<Inbox>,
    control: Option<Arc<WorkerControl>>,
    skip_waiting: bool,
    skip_waiters: Vec<Reply<()>>,
    /// Events dispatched and not answered yet (fetch, lifecycle).
    inflight: u32,
    /// When the last event was dispatched or answered.
    last_event: std::time::Instant,
}

struct Client {
    info: ClientInfo,
    origin: String,
    inbox: Inbox,
    controller: Option<u64>,
    is_window: bool,
}

impl SwShared {
    fn lock(&self) -> MutexGuard<'_, Registry> {
        self.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// End every version (the context is going away).
    pub(crate) fn terminate_all(&self) {
        let mut st = self.lock();
        for v in st.versions.values_mut() {
            if let Some(c) = v.control.take() {
                c.terminate();
            }
        }
    }
}

impl Registry {
    fn vinfo(&self, id: u64) -> Option<VersionInfo> {
        self.versions.get(&id).map(|v| VersionInfo {
            id,
            script_url: v.script_url.clone(),
            state: v.state,
        })
    }

    fn reg(&self, id: u64) -> Option<&Reg> {
        self.regs.iter().find(|r| r.id == id)
    }

    fn reg_mut(&mut self, id: u64) -> Option<&mut Reg> {
        self.regs.iter_mut().find(|r| r.id == id)
    }

    fn rinfo(&self, r: &Reg) -> RegInfo {
        RegInfo {
            id: r.id,
            scope: r.scope.clone(),
            update_via_cache: r.update_via_cache.clone(),
            installing: r.installing.and_then(|v| self.vinfo(v)),
            waiting: r.waiting.and_then(|v| self.vinfo(v)),
            active: r.active.and_then(|v| self.vinfo(v)),
        }
    }

    fn rinfo_id(&self, id: u64) -> Option<RegInfo> {
        self.reg(id).map(|r| self.rinfo(r))
    }

    /// "Match Service Worker Registration": the longest scope that is a
    /// prefix of `url`.
    fn reg_for(&self, url: &Url) -> Option<&Reg> {
        let mut u = url.clone();
        u.set_fragment(None);
        let s = u.as_str();
        self.regs
            .iter()
            .filter(|r| !r.uninstalling && s.starts_with(r.scope.as_str()))
            .max_by_key(|r| r.scope.as_str().len())
    }

    /// Send `mk()` to every listed window and scope of `origin`.
    fn broadcast(&self, origin: &str, mk: impl Fn() -> RemoteTask) {
        for c in self.clients.iter().filter(|c| c.origin == origin) {
            c.inbox.send(mk());
        }
    }

    fn reg_state(&self, id: u64) {
        if let Some(r) = self.reg(id) {
            let info = self.rinfo(r);
            self.broadcast(&r.origin(), || s::reg_state_task(info.clone()));
        }
    }

    fn set_state(&mut self, vid: u64, state: SwState) {
        let Some(v) = self.versions.get_mut(&vid) else {
            return;
        };
        v.state = state;
        if state == SwState::Redundant
            && let Some(c) = v.control.take()
        {
            c.terminate();
        }
        let origin = v.script_url.origin().ascii_serialization();
        if let Some(info) = self.vinfo(vid) {
            self.broadcast(&origin, || s::worker_state_task(info.clone()));
        }
    }

    /// Whether any window is controlled by `vid`.
    fn controls(&self, vid: u64) -> bool {
        self.clients.iter().any(|c| c.controller == Some(vid))
    }

    /// "Clear Registration": every version redundant, the registration
    /// removed.
    fn clear(&mut self, id: u64) {
        let Some(r) = self.reg_mut(id) else { return };
        let vs: Vec<u64> = [r.installing.take(), r.waiting.take(), r.active.take()]
            .into_iter()
            .flatten()
            .collect();
        self.reg_state(id);
        for v in vs {
            self.set_state(v, SwState::Redundant);
            self.versions.remove(&v);
        }
        self.regs.retain(|r| r.id != id);
    }

    /// Ready waiters whose URL now matches registration `id` with an
    /// active worker.
    fn wake_ready(&mut self, id: u64) {
        let waiters = std::mem::take(&mut self.ready);
        for (url, reply) in waiters {
            let hit = self
                .reg_for(&url)
                .filter(|r| r.id == id && r.active.is_some())
                .map(|r| self.rinfo(r));
            match hit {
                Some(info) => reply(info),
                None => self.ready.push((url, reply)),
            }
        }
    }
}

/// The [`SwHost`] of a context's windows and service worker scopes.
pub(crate) struct SwHostImpl {
    ctx: WeakContext,
}

/// The registry host for windows and scopes of `ctx`.
pub(crate) fn host(ctx: &BrowserContext) -> Rc<dyn SwHost> {
    Rc::new(SwHostImpl {
        ctx: ctx.downgrade(),
    })
}

impl SwHostImpl {
    fn shared(&self) -> Option<Arc<SwShared>> {
        self.ctx.upgrade().map(|c| c.service_workers().clone())
    }
}

fn lock(sh: &SwShared) -> MutexGuard<'_, Registry> {
    sh.lock()
}

impl SwHost for SwHostImpl {
    fn add_client(&self, c: ClientReg) {
        let Some(sh) = self.shared() else { return };
        let mut st = lock(&sh);
        st.clients.retain(|x| x.info.id != c.info.id);
        st.clients.push(Client {
            info: c.info,
            origin: c.origin,
            inbox: c.inbox,
            controller: c.controller,
            is_window: c.is_window,
        });
    }

    fn remove_client(&self, id: &str) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let sh = ctx.service_workers().clone();
        let mut st = lock(&sh);
        let Some(i) = st.clients.iter().position(|c| c.info.id == id) else {
            return;
        };
        let c = st.clients.remove(i);
        let Some(vid) = c.controller else { return };
        if st.controls(vid) {
            return;
        }
        let Some(reg) = st.versions.get(&vid).map(|v| v.reg) else {
            return;
        };
        let (uninstalling, waiting) = st
            .reg(reg)
            .map_or((false, false), |r| (r.uninstalling, r.waiting.is_some()));
        if uninstalling {
            st.clear(reg);
        } else if waiting {
            drop(st);
            tokio::task::spawn_local(try_activate(ctx, reg));
        }
    }

    fn register(
        &self,
        client: ClientInfo,
        script: Url,
        scope: Url,
        update_via_cache: String,
        reply: Reply<Result<RegInfo, SwError>>,
    ) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        tokio::task::spawn_local(async move {
            job_register(ctx, client, script, scope, update_via_cache, reply).await;
        });
    }

    fn update(&self, reg: u64, reply: Reply<Result<RegInfo, SwError>>) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        tokio::task::spawn_local(async move {
            job_update(ctx, reg, None, Some(reply)).await;
        });
    }

    fn unregister(&self, reg: u64, reply: Reply<bool>) {
        let Some(sh) = self.shared() else {
            reply(false);
            return;
        };
        let mut st = lock(&sh);
        let live = st.reg(reg).is_some_and(|r| !r.uninstalling);
        if !live {
            drop(st);
            reply(false);
            return;
        }
        if let Some(r) = st.reg_mut(reg) {
            r.uninstalling = true;
        }
        let vs: Vec<u64> = st
            .reg(reg)
            .map(|r| {
                [r.installing, r.waiting, r.active]
                    .into_iter()
                    .flatten()
                    .collect()
            })
            .unwrap_or_default();
        if !vs.iter().any(|v| st.controls(*v)) {
            st.clear(reg);
        }
        drop(st);
        reply(true);
    }

    fn registration_for(&self, url: &Url) -> Option<RegInfo> {
        let sh = self.shared()?;
        let st = lock(&sh);
        st.reg_for(url).map(|r| st.rinfo(r))
    }

    fn registrations(&self, origin: &str) -> Vec<RegInfo> {
        let Some(sh) = self.shared() else {
            return Vec::new();
        };
        let st = lock(&sh);
        st.regs
            .iter()
            .filter(|r| !r.uninstalling && r.origin() == origin)
            .map(|r| st.rinfo(r))
            .collect()
    }

    fn registration(&self, id: u64) -> Option<RegInfo> {
        let sh = self.shared()?;
        let st = lock(&sh);
        st.rinfo_id(id)
    }

    fn version(&self, id: u64) -> Option<VersionInfo> {
        let sh = self.shared()?;
        let st = lock(&sh);
        st.vinfo(id)
    }

    fn has_registrations(&self, origin: &str) -> bool {
        self.shared()
            .is_some_and(|sh| lock(&sh).regs.iter().any(|r| r.origin() == origin))
    }

    fn when_ready(&self, url: Url, reply: Reply<RegInfo>) {
        let Some(sh) = self.shared() else { return };
        let mut st = lock(&sh);
        let hit = st
            .reg_for(&url)
            .filter(|r| r.active.is_some())
            .map(|r| st.rinfo(r));
        match hit {
            Some(info) => {
                drop(st);
                reply(info);
            }
            None => st.ready.push((url, reply)),
        }
    }

    fn worker_inbox(&self, version: u64) -> Option<Inbox> {
        // A message is an event: it starts a stopped version.
        let ctx = self.ctx.upgrade()?;
        let sh = ctx.service_workers().clone();
        running_inbox(&ctx, &sh, version, false)
    }

    fn dispatch_fetch(&self, version: u64, req: SwRequest) -> LocalFuture<FetchOutcome> {
        let ctx = self.ctx.upgrade();
        Box::pin(async move {
            match ctx {
                Some(ctx) => dispatch_fetch(&ctx, version, req).await,
                None => FetchOutcome::Fallback,
            }
        })
    }

    fn skip_waiting(&self, version: u64, reply: Reply<()>) {
        let Some(ctx) = self.ctx.upgrade() else {
            return;
        };
        let sh = ctx.service_workers().clone();
        let mut st = lock(&sh);
        let Some(v) = st.versions.get_mut(&version) else {
            drop(st);
            reply(());
            return;
        };
        v.skip_waiting = true;
        if v.state == SwState::Installed {
            v.skip_waiters.push(reply);
            let reg = v.reg;
            drop(st);
            tokio::task::spawn_local(try_activate(ctx, reg));
        } else {
            drop(st);
            reply(());
        }
    }

    fn claim(&self, version: u64, reply: Reply<Result<(), SwError>>) {
        let Some(sh) = self.shared() else { return };
        let mut st = lock(&sh);
        let reg = st.versions.get(&version).map(|v| (v.reg, v.state));
        let ok = reg.and_then(|(reg, state)| {
            (matches!(state, SwState::Activating | SwState::Activated)
                && st.reg(reg).is_some_and(|r| r.active == Some(version)))
            .then_some(reg)
        });
        let Some(reg) = ok else {
            drop(st);
            reply(Err(SwError::new(
                "InvalidStateError",
                "Only the active worker can claim clients.",
            )));
            return;
        };
        let info = st.vinfo(version);
        let targets: Vec<usize> = (0..st.clients.len())
            .filter(|&i| {
                let c = &st.clients[i];
                c.is_window
                    && c.controller != Some(version)
                    && st.reg_for(&c.info.url).is_some_and(|r| r.id == reg)
            })
            .collect();
        for i in targets {
            st.clients[i].controller = Some(version);
            if let Some(info) = &info {
                st.clients[i].inbox.send(s::controller_task(info.clone()));
            }
        }
        drop(st);
        reply(Ok(()));
    }

    fn clients(&self, version: u64, include_uncontrolled: bool) -> Vec<ClientInfo> {
        let Some(sh) = self.shared() else {
            return Vec::new();
        };
        let st = lock(&sh);
        let Some(origin) = st
            .versions
            .get(&version)
            .map(|v| v.script_url.origin().ascii_serialization())
        else {
            return Vec::new();
        };
        st.clients
            .iter()
            .filter(|c| {
                c.is_window
                    && c.origin == origin
                    && (include_uncontrolled || c.controller == Some(version))
            })
            .map(|c| c.info.clone())
            .collect()
    }

    fn client(&self, id: &str) -> Option<(ClientInfo, String, Inbox)> {
        let sh = self.shared()?;
        let st = lock(&sh);
        st.clients
            .iter()
            .find(|c| c.is_window && c.info.id == id)
            .map(|c| (c.info.clone(), c.origin.clone(), c.inbox.clone()))
    }

    fn caches(&self, origin: &str) -> CacheStorageRef {
        match self.shared() {
            Some(sh) => lock(&sh)
                .caches
                .entry(origin.to_owned())
                .or_default()
                .clone(),
            None => CacheStorageRef::default(),
        }
    }

    fn schedule_update(&self, version: u64) {
        let ctx = self.ctx.clone();
        tokio::task::spawn_local(async move {
            tokio::time::sleep(SOFT_UPDATE_DELAY).await;
            let Some(ctx) = ctx.upgrade() else { return };
            // Chrome drops the scheduled update when the version is no
            // longer its registration's newest.
            let reg = {
                let st = lock(ctx.service_workers());
                st.versions.get(&version).map(|v| v.reg).filter(|&r| {
                    st.reg(r)
                        .is_some_and(|r| !r.uninstalling && r.newest() == Some(version))
                })
            };
            if let Some(reg) = reg {
                job_update(ctx, reg, Some(version), None).await;
            }
        });
    }
}

// ---------------------------------------------------------------------------
// Fetch
// ---------------------------------------------------------------------------

async fn dispatch_fetch(ctx: &BrowserContext, version: u64, req: SwRequest) -> FetchOutcome {
    let sh = &**ctx.service_workers();
    let inbox = loop {
        let notified = sh.changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let st = lock(sh);
            let Some(v) = st.versions.get(&version) else {
                return FetchOutcome::Fallback;
            };
            if !v.has_fetch {
                return FetchOutcome::Fallback;
            }
            match v.state {
                SwState::Activated => {
                    drop(st);
                    match running_inbox(ctx, sh, version, true) {
                        Some(i) => break i,
                        None => return FetchOutcome::Fallback,
                    }
                }
                SwState::Redundant => return FetchOutcome::Fallback,
                _ => {}
            }
        }
        notified.await;
    };
    let (tx, rx) = oneshot::channel();
    let sent = inbox.send(s::fetch_task(
        req,
        Box::new(move |o| {
            let _ = tx.send(o);
        }),
    ));
    if !sent {
        event_done(sh, version);
        return FetchOutcome::Fallback;
    }
    let r = rx.await.unwrap_or(FetchOutcome::Fallback);
    event_done(sh, version);
    r
}

/// The inbox of version `vid`, starting its thread again when it was
/// stopped for being idle. `event`: an answered event follows
/// ([`event_done`] ends it); otherwise the call itself is the event.
fn running_inbox(ctx: &BrowserContext, sh: &SwShared, vid: u64, event: bool) -> Option<Inbox> {
    let _one = sh.restart.lock().unwrap_or_else(|e| e.into_inner());
    let start = {
        let mut st = lock(sh);
        let v = st.versions.get_mut(&vid)?;
        v.last_event = std::time::Instant::now();
        if event {
            v.inflight += 1;
        }
        if let Some(i) = &v.inbox {
            return Some(i.clone());
        }
        if !matches!(v.state, SwState::Installed | SwState::Activated) {
            if event {
                v.inflight -= 1;
            }
            return None;
        }
        (v.reg, v.script_url.clone(), v.source.clone())
    };
    // The script runs again from the version's copy (no fetch); there is
    // no install or activate event.
    let (reg, script, source) = start;
    let _ = start_version(ctx, sh, vid, reg, &script, source, &script);
    lock(sh).versions.get(&vid).and_then(|v| v.inbox.clone())
}

/// An event dispatched to `vid` was answered.
fn event_done(sh: &SwShared, vid: u64) {
    if let Some(v) = lock(sh).versions.get_mut(&vid) {
        v.inflight = v.inflight.saturating_sub(1);
        v.last_event = std::time::Instant::now();
    }
}

/// Runs on version `vid`'s own thread: stop it once it has been idle for
/// [`IDLE_TIMEOUT`] with no event in flight, unless `inspected` (a
/// DevTools session is attached; Chrome keeps such a worker running).
pub(crate) async fn idle_watch(ctx: WeakContext, vid: u64, inspected: impl Fn() -> bool) {
    loop {
        let wait = {
            let Some(ctx) = ctx.upgrade() else { return };
            let sh = ctx.service_workers().clone();
            let mut st = lock(&sh);
            let Some(v) = st.versions.get_mut(&vid) else {
                return;
            };
            if v.inbox.is_none() {
                return;
            }
            let idle = v.last_event.elapsed();
            let limit = sh.idle_timeout();
            if inspected() {
                v.last_event = std::time::Instant::now();
                limit
            } else if v.inflight == 0
                && idle >= limit
                && matches!(v.state, SwState::Installed | SwState::Activated)
            {
                v.inbox = None;
                if let Some(c) = v.control.take() {
                    c.terminate();
                }
                return;
            } else if v.inflight > 0 {
                limit
            } else {
                limit.saturating_sub(idle)
            }
        };
        tokio::time::sleep(wait.max(Duration::from_millis(1))).await;
    }
}

/// The scope of registration `reg`.
pub(crate) fn scope_of(ctx: &BrowserContext, reg: u64) -> Option<String> {
    lock(ctx.service_workers())
        .reg(reg)
        .map(|r| r.scope.to_string())
}

/// The controller of a navigation to `url` (the matching registration's
/// active worker, activating or activated) and whether it handles fetch.
pub(crate) fn navigation_controller(
    ctx: &BrowserContext,
    url: &Url,
) -> Option<(VersionInfo, bool)> {
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let st = lock(ctx.service_workers());
    let r = st.reg_for(url)?;
    let vid = r.active?;
    let v = st.versions.get(&vid)?;
    matches!(v.state, SwState::Activating | SwState::Activated)
        .then(|| (st.vinfo(vid), v.has_fetch))
        .and_then(|(i, f)| i.map(|i| (i, f)))
}

/// A navigation request through `version`'s `fetch` event.
pub(crate) async fn navigation_fetch(
    ctx: &BrowserContext,
    version: u64,
    req: SwRequest,
) -> FetchOutcome {
    dispatch_fetch(ctx, version, req).await
}

// ---------------------------------------------------------------------------
// Jobs
// ---------------------------------------------------------------------------

async fn job_register(
    ctx: BrowserContext,
    client: ClientInfo,
    script: Url,
    scope: Url,
    uvc: String,
    reply: Reply<Result<RegInfo, SwError>>,
) {
    let sh = ctx.service_workers().clone();
    let _job = sh.jobs.lock().await;
    let (reg, created) = {
        let mut st = lock(&sh);
        let found = st
            .regs
            .iter()
            .find(|r| r.scope == scope && !r.uninstalling)
            .map(|r| (r.id, r.newest(), r.update_via_cache.clone()));
        match found {
            Some((id, newest, old_uvc)) => {
                let same = newest
                    .and_then(|n| st.versions.get(&n))
                    .is_some_and(|v| v.script_url == script);
                if same && old_uvc == uvc {
                    let info = st.rinfo_id(id);
                    drop(st);
                    if let Some(info) = info {
                        reply(Ok(info));
                    }
                    return;
                }
                if let Some(r) = st.reg_mut(id) {
                    r.update_via_cache = uvc;
                }
                (id, false)
            }
            None => {
                st.next += 1;
                let id = st.next;
                st.regs.push(Reg {
                    id,
                    scope: scope.clone(),
                    update_via_cache: uvc,
                    installing: None,
                    waiting: None,
                    active: None,
                    uninstalling: false,
                });
                (id, true)
            }
        }
    };
    let prefix = format!(
        "Failed to register a ServiceWorker for scope ('{scope}') with script ('{script}'): "
    );
    run_update(
        &ctx,
        &sh,
        reg,
        script,
        client.url,
        prefix,
        created,
        Some(reply),
    )
    .await;
}

/// `update()` (with a reply) or a soft update after a navigation.
async fn job_update(
    ctx: BrowserContext,
    reg: u64,
    expect: Option<u64>,
    reply: Option<Reply<Result<RegInfo, SwError>>>,
) {
    let sh = ctx.service_workers().clone();
    let _job = sh.jobs.lock().await;
    let found = {
        let st = lock(&sh);
        st.reg(reg).filter(|r| !r.uninstalling).and_then(|r| {
            let n = r.newest()?;
            if expect.is_some_and(|e| e != n) {
                return None;
            }
            Some((r.scope.clone(), st.versions.get(&n)?.script_url.clone()))
        })
    };
    let Some((scope, script)) = found else {
        if let Some(reply) = reply {
            reply(Err(SwError::new(
                "InvalidStateError",
                "Failed to update a ServiceWorker: The registration is not found.",
            )));
        }
        return;
    };
    let prefix = format!(
        "Failed to update a ServiceWorker for scope ('{scope}') with script ('{script}'): "
    );
    let creator = scope.clone();
    run_update(&ctx, &sh, reg, script, creator, prefix, false, reply).await;
}

fn is_js_mime(m: &str) -> bool {
    matches!(
        m,
        "text/javascript"
            | "application/javascript"
            | "application/x-javascript"
            | "application/ecmascript"
            | "application/x-ecmascript"
            | "text/ecmascript"
            | "text/jscript"
            | "text/livescript"
            | "text/x-ecmascript"
            | "text/x-javascript"
            | "text/javascript1.0"
            | "text/javascript1.1"
            | "text/javascript1.2"
            | "text/javascript1.3"
            | "text/javascript1.4"
            | "text/javascript1.5"
    )
}

/// "Update": fetch and check the script, compare it with the newest
/// version, run a new version and install it. `reply` gets the job's
/// result (at Install, or on failure / no change).
#[allow(clippy::too_many_arguments)]
async fn run_update(
    ctx: &BrowserContext,
    sh: &Arc<SwShared>,
    reg: u64,
    script: Url,
    creator: Url,
    prefix: String,
    created: bool,
    reply: Option<Reply<Result<RegInfo, SwError>>>,
) {
    let reply = std::cell::RefCell::new(reply);
    let take = || reply.borrow_mut().take();
    let fail = |name: &'static str, m: String| {
        if created {
            let mut st = lock(sh);
            if st.reg(reg).is_some_and(|r| r.newest().is_none()) {
                st.regs.retain(|r| r.id != reg);
            }
        }
        if let Some(r) = take() {
            r(Err(SwError::new(name, format!("{prefix}{m}"))));
        }
    };
    let scope = match lock(sh).reg(reg) {
        Some(r) => r.scope.clone(),
        None => return,
    };
    let mut req = NetRequest::get(script.clone(), Destination::Script, Some(creator.clone()));
    req.mode = "same-origin";
    req.headers.push(("Service-Worker".into(), "script".into()));
    let resp = match ctx.fetch(req).await {
        Ok(r) => r,
        Err(_) => {
            return fail(
                "TypeError",
                "An unknown error occurred when fetching the script.".into(),
            );
        }
    };
    if !(200..300).contains(&resp.status) {
        return fail(
            "TypeError",
            format!(
                "A bad HTTP response code ({}) was received when fetching the script.",
                resp.status
            ),
        );
    }
    let ct = resp.header("content-type").unwrap_or("").to_owned();
    let mime = ct
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    if mime.is_empty() {
        return fail(
            "SecurityError",
            "The script does not have a MIME type.".into(),
        );
    }
    if !is_js_mime(&mime) {
        return fail(
            "SecurityError",
            format!("The script has an unsupported MIME type ('{mime}')."),
        );
    }
    let max = match resp.header("service-worker-allowed") {
        Some(a) => resp.url.join(a).ok(),
        None => resp.url.join("./").ok(),
    };
    if let Some(max) = max
        && !scope.path().starts_with(max.path())
    {
        return fail(
            "SecurityError",
            format!(
                "The path of the provided scope ('{scope}') is not under the max scope allowed ('{max}'). Adjust the scope, move the Service Worker script, or use the Service-Worker-Allowed HTTP header to allow the scope."
            ),
        );
    }
    let text = lb_webapi::rt::script::script_source(&resp.body, Some(&ct));

    // Byte-for-byte the newest version's script: nothing to do.
    let vid = {
        let mut st = lock(sh);
        let same = st
            .reg(reg)
            .and_then(Reg::newest)
            .and_then(|n| st.versions.get(&n))
            .is_some_and(|v| v.script_url == script && v.source == text);
        if same {
            let info = st.rinfo_id(reg);
            drop(st);
            if let (Some(info), Some(r)) = (info, take()) {
                r(Ok(info));
            }
            return;
        }
        st.next += 1;
        let vid = st.next;
        st.versions.insert(
            vid,
            Version {
                reg,
                script_url: script.clone(),
                state: SwState::Parsed,
                source: text.clone(),
                has_fetch: false,
                inbox: None,
                control: None,
                skip_waiting: false,
                skip_waiters: Vec::new(),
                inflight: 0,
                last_event: std::time::Instant::now(),
            },
        );
        vid
    };

    // Run the script on its own thread.
    let evaluated = start_version(ctx, sh, vid, reg, &script, text, &creator);
    match evaluated.await {
        Ok(Ok(has_fetch)) => {
            if let Some(v) = lock(sh).versions.get_mut(&vid) {
                v.has_fetch = has_fetch;
            }
        }
        _ => {
            {
                let mut st = lock(sh);
                if let Some(mut v) = st.versions.remove(&vid)
                    && let Some(c) = v.control.take()
                {
                    c.terminate();
                }
            }
            return fail("TypeError", "ServiceWorker script evaluation failed".into());
        }
    }
    install(ctx, sh, reg, vid, take()).await;
}

/// Start version `vid`'s thread; resolves once its script ran.
fn start_version(
    ctx: &BrowserContext,
    sh: &SwShared,
    vid: u64,
    reg: u64,
    script: &Url,
    source: String,
    creator: &Url,
) -> oneshot::Receiver<Result<bool, ()>> {
    let (tx, rx) = oneshot::channel();
    let (inbox, inbox_rx) = lb_webapi::rt::worker::inbox();
    let control = WorkerControl::new(inbox.clone());
    if let Some(v) = lock(sh).versions.get_mut(&vid) {
        v.inbox = Some(inbox.clone());
        v.control = Some(control.clone());
    }
    let start = WorkerStart {
        kind: WorkerKind::Service,
        url: script.clone(),
        source: Some(source),
        module: false,
        name: String::new(),
        origin: script.origin().ascii_serialization(),
        creator: creator.clone(),
        persona: ctx.persona().clone(),
        profile: ctx.profile().cloned(),
        virtual_time: ctx.options().virtual_time,
        inbox,
        inbox_rx,
        control,
        parent: None,
        service: Some(ServiceStart {
            version: vid,
            registration: reg,
            evaluated: Box::new(move |r| {
                let _ = tx.send(r);
            }),
        }),
    };
    crate::worker::spawn(ctx.clone(), start, crate::worker::Inspect::Yes(None));
    rx
}

/// Send a lifecycle event to `vid` and wait for its `waitUntil` promises.
async fn lifecycle(sh: &SwShared, vid: u64, kind: &'static str, after: Vec<Reply<()>>) -> bool {
    let inbox = {
        let mut st = lock(sh);
        let v = st.versions.get_mut(&vid);
        let inbox = v.as_ref().and_then(|v| v.inbox.clone());
        if let (Some(v), Some(_)) = (v, &inbox) {
            v.inflight += 1;
            v.last_event = std::time::Instant::now();
        }
        inbox
    };
    let Some(inbox) = inbox else { return false };
    let (tx, rx) = oneshot::channel();
    inbox.send(s::lifecycle_task(
        kind,
        Box::new(move |ok| {
            let _ = tx.send(ok);
        }),
    ));
    // Replies that send to the same inbox land after the event.
    for r in after {
        r(());
    }
    let ok = rx.await.unwrap_or(false);
    event_done(sh, vid);
    ok
}

/// "Install".
async fn install(
    ctx: &BrowserContext,
    sh: &Arc<SwShared>,
    reg: u64,
    vid: u64,
    reply: Option<Reply<Result<RegInfo, SwError>>>,
) {
    let origin = {
        let mut st = lock(sh);
        let Some(r) = st.reg_mut(reg) else { return };
        let old = r.installing.replace(vid);
        let origin = r.origin();
        st.reg_state(reg);
        if let Some(old) = old {
            st.set_state(old, SwState::Redundant);
        }
        st.set_state(vid, SwState::Installing);
        let info = st.rinfo_id(reg);
        drop(st);
        if let (Some(info), Some(reply)) = (info, reply) {
            reply(Ok(info));
        }
        let st = lock(sh);
        st.broadcast(&origin, || s::update_found_task(reg));
        origin
    };
    let _ = origin;
    let ok = lifecycle(sh, vid, "install", Vec::new()).await;
    {
        let mut st = lock(sh);
        let still = st.reg(reg).is_some_and(|r| r.installing == Some(vid));
        if !still {
            return;
        }
        if !ok {
            if let Some(r) = st.reg_mut(reg) {
                r.installing = None;
            }
            st.reg_state(reg);
            st.set_state(vid, SwState::Redundant);
            st.versions.remove(&vid);
            if st.reg(reg).is_some_and(|r| r.newest().is_none()) {
                st.regs.retain(|r| r.id != reg);
            }
            return;
        }
        let old = st.reg_mut(reg).and_then(|r| r.waiting.take());
        if let Some(old) = old {
            st.set_state(old, SwState::Redundant);
            st.versions.remove(&old);
        }
        if let Some(r) = st.reg_mut(reg) {
            r.waiting = Some(vid);
            r.installing = None;
        }
        st.reg_state(reg);
        st.set_state(vid, SwState::Installed);
    }
    tokio::task::spawn_local(try_activate(ctx.clone(), reg));
}

/// "Try Activate": activate the waiting version when the active one
/// controls no client (or `skipWaiting()` was called).
async fn try_activate(ctx: BrowserContext, reg: u64) {
    let sh = ctx.service_workers().clone();
    let go = {
        let st = lock(&sh);
        st.reg(reg).is_some_and(|r| {
            r.waiting.is_some_and(|w| {
                r.active.is_none_or(|a| !st.controls(a))
                    || st.versions.get(&w).is_some_and(|v| v.skip_waiting)
            })
        })
    };
    if go {
        activate(&sh, reg).await;
    }
}

/// "Activate".
async fn activate(sh: &SwShared, reg: u64) {
    let (vid, skip) = {
        let mut st = lock(sh);
        let Some(r) = st.reg_mut(reg) else { return };
        let Some(vid) = r.waiting.take() else { return };
        let old = r.active.replace(vid);
        if let Some(old) = old {
            st.set_state(old, SwState::Redundant);
        }
        st.reg_state(reg);
        st.set_state(vid, SwState::Activating);
        // Clients of the old version now use the new one.
        if let Some(info) = st.vinfo(vid) {
            for c in st.clients.iter_mut() {
                if old.is_some() && c.controller == old {
                    c.controller = Some(vid);
                    c.inbox.send(s::controller_task(info.clone()));
                }
            }
        }
        if let Some(old) = old {
            st.versions.remove(&old);
        }
        st.wake_ready(reg);
        let skip = st
            .versions
            .get_mut(&vid)
            .map(|v| std::mem::take(&mut v.skip_waiters))
            .unwrap_or_default();
        (vid, skip)
    };
    sh.changed.notify_waiters();
    // `skipWaiting()` promises resolve after the activate event was
    // dispatched (Chrome).
    let _ = lifecycle(sh, vid, "activate", skip).await;
    {
        let mut st = lock(sh);
        if st
            .versions
            .get(&vid)
            .is_some_and(|v| v.state == SwState::Activating)
        {
            st.set_state(vid, SwState::Activated);
        }
    }
    sh.changed.notify_waiters();
}
