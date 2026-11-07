//! Session history of a page (ADR 0045): the list `history.length`,
//! `Page.getNavigationHistory` and `Page.navigateToHistoryEntry` share.
//!
//! Every entry belongs to a document (`doc`, the navigation sequence that
//! made it). Moving between entries of one document is a same-document
//! traversal (`popstate`, `hashchange`); moving to another document's
//! entry loads its URL again and gives it back its `history.state`.
//! Only the top-level frame takes part: a frame has a history of its own
//! (open item in ADR 0045).

use std::rc::{Rc, Weak};

use lb_cdp::PageEvent;
use lb_webapi::rt::{EngineJs, SessionHost};
use url::Url;

use crate::browser::WorkerState;
use crate::page::PageState;

/// One entry of the session history.
#[derive(Debug, Clone)]
pub(crate) struct Entry {
    pub id: i64,
    pub url: Url,
    /// The URL the user (or `Page.navigate`) asked for; same-document
    /// entries inherit it.
    pub user_typed: String,
    /// The title when the entry was left; the current entry's is live.
    pub title: String,
    /// CDP `Page.TransitionType`.
    pub transition: String,
    /// `history.state` as JSON.
    pub state: Option<String>,
    /// The document that holds the entry (0: none yet).
    pub doc: u64,
}

/// How a committed document relates to the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NavKind {
    /// A new entry after the current one (or replacing it).
    New,
    /// The current entry loads again.
    Reload,
    /// `Page.reload` with `ignoreCache`.
    ReloadBypassingCache,
    /// A traversal to the entry with this id.
    History(i64),
}

#[derive(Debug, Default)]
pub(crate) struct Session {
    pub entries: Vec<Entry>,
    pub current: usize,
    next_id: i64,
    /// The live document's title as of the last [`Session::sync_title`]:
    /// the current entry takes a new title when the document's changes,
    /// not when the entry is left (a traversal inside a document does not
    /// change it).
    seen_title: String,
}

impl Session {
    /// An empty history, or one holding the entry of the `about:blank`
    /// document a page that skips it would have had.
    pub(crate) fn new(blank: bool) -> Session {
        let mut s = Session::default();
        if blank && let Ok(url) = Url::parse("about:blank") {
            s.push(url, "about:blank".into(), "typed".into(), 0);
        }
        s
    }

    fn push(&mut self, url: Url, user_typed: String, transition: String, doc: u64) {
        self.next_id += 1;
        self.entries.push(Entry {
            id: self.next_id,
            url,
            user_typed,
            title: String::new(),
            transition,
            state: None,
            doc,
        });
        self.current = self.entries.len() - 1;
    }

    pub(crate) fn current_entry(&self) -> Option<&Entry> {
        self.entries.get(self.current)
    }

    /// The document's title is `live`: if it changed, the current entry
    /// has it.
    pub(crate) fn sync_title(&mut self, live: &str) {
        if live != self.seen_title {
            self.seen_title = live.to_string();
            if let Some(e) = self.entries.get_mut(self.current) {
                e.title = live.to_string();
            }
        }
    }

    /// A new document committed at `url` (after redirects).
    pub(crate) fn commit(&mut self, kind: &NavKind, c: Commit<'_>) {
        // The new document brings its own title.
        self.seen_title.clear();
        match kind {
            NavKind::New => {
                let same_url = c.requested_same_as_current(self);
                if (c.replace || same_url) && !self.entries.is_empty() {
                    let e = &mut self.entries[self.current];
                    e.url = c.url.clone();
                    e.user_typed = c.user_typed.to_string();
                    e.transition = c.transition.to_string();
                    e.state = None;
                    e.title = String::new();
                    e.doc = c.doc;
                } else {
                    self.entries.truncate(self.current + 1);
                    self.push(
                        c.url.clone(),
                        c.user_typed.to_string(),
                        c.transition.to_string(),
                        c.doc,
                    );
                }
            }
            NavKind::Reload | NavKind::ReloadBypassingCache => {
                // `history.go(0)` keeps the entry's transition (no value).
                let transition = if c.transition.is_empty() {
                    "typed"
                } else {
                    c.transition
                };
                if let Some(e) = self.entries.get_mut(self.current) {
                    e.url = c.url.clone();
                    if !c.transition.is_empty() {
                        e.transition = c.transition.to_string();
                    }
                    e.title = String::new();
                    e.doc = c.doc;
                } else {
                    self.push(
                        c.url.clone(),
                        c.user_typed.to_string(),
                        transition.into(),
                        c.doc,
                    );
                }
            }
            NavKind::History(id) => {
                if let Some(i) = self.entries.iter().position(|e| e.id == *id) {
                    self.current = i;
                    let e = &mut self.entries[i];
                    e.url = c.url.clone();
                    e.title = String::new();
                    e.doc = c.doc;
                }
            }
        }
    }

    /// A same-document entry: `pushState`, `replaceState`, a fragment
    /// navigation. `title` is the document's.
    pub(crate) fn record(&mut self, url: &Url, state: Option<String>, replace: bool, title: &str) {
        let Some(cur) = self.entries.get_mut(self.current) else {
            return;
        };
        if replace {
            cur.url = url.clone();
            cur.state = state;
            return;
        }
        let (user_typed, doc) = (cur.user_typed.clone(), cur.doc);
        self.entries.truncate(self.current + 1);
        self.push(url.clone(), user_typed, "link".into(), doc);
        let e = &mut self.entries[self.current];
        e.state = state;
        // The new entry starts with the document's title; its own title
        // changes from here on belong to it.
        e.title = title.to_string();
        self.seen_title = title.to_string();
    }

    /// The index `delta` entries from the current one.
    pub(crate) fn index_at(&self, delta: i32) -> Option<usize> {
        let t = self.current as i64 + i64::from(delta);
        (t >= 0 && (t as usize) < self.entries.len()).then_some(t as usize)
    }

    /// `Page.resetNavigationHistory`: only the current entry stays.
    pub(crate) fn reset(&mut self) {
        if let Some(e) = self.entries.get(self.current).cloned() {
            self.entries = vec![e];
            self.current = 0;
        }
    }
}

/// What a commit tells the history.
pub(crate) struct Commit<'a> {
    /// The URL after redirects.
    pub url: &'a Url,
    /// The URL that was requested.
    pub requested: &'a Url,
    pub user_typed: &'a str,
    pub transition: &'a str,
    pub doc: u64,
    pub replace: bool,
    /// A form POST never counts as the same URL.
    pub post: bool,
}

impl Commit<'_> {
    /// Chrome turns a navigation to the URL the page is on into a
    /// replacement of the entry.
    fn requested_same_as_current(&self, s: &Session) -> bool {
        !self.post && s.current_entry().is_some_and(|e| e.url == *self.requested)
    }
}

impl PageState {
    /// The title of the live document.
    pub(crate) fn live_title(&self) -> Option<String> {
        self.window().map(|w| w.with_doc(|d| d.title()))
    }

    /// The current entry takes the live document's title if it changed;
    /// called before the history is read or changes.
    pub(crate) fn session_leave(&self) {
        if let Some(t) = self.live_title() {
            self.history.borrow_mut().sync_title(&t);
        }
    }

    /// `Page.getNavigationHistory`: the current index and the entries.
    pub(crate) fn session_snapshot(&self) -> (usize, Vec<Entry>) {
        self.session_leave();
        let s = self.history.borrow();
        (s.current, s.entries.clone())
    }
}

/// The page's [`SessionHost`] for its top-level windows.
struct PageSession {
    page: Weak<PageState>,
}

impl SessionHost for PageSession {
    fn length(&self) -> u32 {
        self.page
            .upgrade()
            .map_or(1, |p| p.history.borrow().entries.len() as u32)
    }

    fn state(&self) -> Option<String> {
        let p = self.page.upgrade()?;
        let s = p.history.borrow();
        s.current_entry().and_then(|e| e.state.clone())
    }

    fn record(&self, url: &Url, state: Option<String>, replace: bool, fragment: bool) {
        let Some(p) = self.page.upgrade() else { return };
        p.session_leave();
        let title = p.live_title().unwrap_or_default();
        let agent = p.agent();
        if let Some(a) = &agent {
            a.emit(PageEvent::FrameStartedLoading {
                frame_id: a.target.clone(),
            });
        }
        p.history.borrow_mut().record(url, state, replace, &title);
        p.update_life(|l| l.url = url.to_string());
        if let Some(a) = &agent {
            a.same_document(url);
            a.emit(PageEvent::NavigatedWithinDocument {
                frame_id: a.target.clone(),
                url: url.to_string(),
                navigation_type: if fragment { "fragment" } else { "historyApi" }.into(),
            });
            a.emit(PageEvent::FrameStoppedLoading {
                frame_id: a.target.clone(),
            });
        }
    }
}

pub(crate) fn host(page: &Rc<PageState>) -> Rc<dyn SessionHost> {
    Rc::new(PageSession {
        page: Rc::downgrade(page),
    })
}

/// A traversal by `delta` entries, from script (`history.go`) or CDP.
/// Returns `false` when there is no such entry. A same-document target
/// runs here; a document of another entry loads (the future runs until
/// `load`, like a navigation).
pub(crate) fn traverse(ws: &Rc<WorkerState>, page: &Rc<PageState>, delta: i32) -> bool {
    if delta == 0 {
        let Some(url) = page.window().map(|w| w.url()) else {
            return false;
        };
        page.reloading.set(true);
        let (ws, page) = (ws.clone(), page.clone());
        tokio::task::spawn_local(async move {
            let mut target = crate::nav::Target::reload(url, false);
            target.transition = String::new();
            let _ = crate::nav::navigate(ws, page, target).await;
        });
        return true;
    }
    let id = {
        let s = page.history.borrow();
        s.index_at(delta).map(|i| s.entries[i].id)
    };
    match id {
        Some(id) => traverse_to(ws, page, id, None),
        None => false,
    }
}

/// Go to the entry `id`. `committed` is told when a document load
/// commits (never for a same-document move: the caller learns it from
/// the return value).
pub(crate) fn traverse_to(
    ws: &Rc<WorkerState>,
    page: &Rc<PageState>,
    id: i64,
    committed: Option<
        tokio::sync::oneshot::Sender<Result<crate::NavResult, crate::context::PageError>>,
    >,
) -> bool {
    let (url, state, same_doc) = {
        let s = page.history.borrow();
        let Some(i) = s.entries.iter().position(|e| e.id == id) else {
            return false;
        };
        let cur_doc = s.current_entry().map(|e| e.doc);
        (
            s.entries[i].url.clone(),
            s.entries[i].state.clone(),
            cur_doc == Some(s.entries[i].doc) && page.window().is_some(),
        )
    };
    if same_doc && let Some(w) = page.window() {
        // Same document: the history moves, then the document hears of it.
        page.session_leave();
        let agent = page.agent();
        if let Some(a) = &agent {
            a.nav_started_same_document(&url, "historySameDocument");
            a.emit(PageEvent::FrameStartedLoading {
                frame_id: a.target.clone(),
            });
        }
        {
            let mut s = page.history.borrow_mut();
            if let Some(i) = s.entries.iter().position(|e| e.id == id) {
                s.current = i;
            }
        }
        page.update_life(|l| l.url = url.to_string());
        if let Some(a) = &agent {
            a.same_document(&url);
            a.emit(PageEvent::NavigatedWithinDocument {
                frame_id: a.target.clone(),
                url: url.to_string(),
                // Chrome says `fragment` for every traversal inside a document.
                navigation_type: "fragment".into(),
            });
        }
        let realm = w.realm();
        ws.with_engine(|engine| {
            let mut js = EngineJs { engine, realm };
            w.traverse_same_document(&mut js, url.clone(), state);
            js.engine.perform_microtask_checkpoint();
        });
        if let Some(a) = &agent {
            a.emit(PageEvent::FrameStoppedLoading {
                frame_id: a.target.clone(),
            });
        }
        if let Some(tx) = committed {
            drop(tx);
        }
        return true;
    }
    let (ws, page) = (ws.clone(), page.clone());
    tokio::task::spawn_local(async move {
        let target = crate::nav::Target::history(url, id);
        let r = match committed {
            Some(tx) => crate::nav::navigate_committed(ws, page, target, tx).await,
            None => crate::nav::navigate(ws, page, target).await,
        };
        let _ = r;
    });
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    fn u(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn commit(s: &mut Session, kind: &NavKind, url: &str, transition: &str, doc: u64) {
        let url = u(url);
        s.commit(
            kind,
            Commit {
                url: &url,
                requested: &url,
                user_typed: url.as_str(),
                transition,
                doc,
                replace: false,
                post: false,
            },
        );
    }

    fn view(s: &Session) -> Vec<(String, String, String)> {
        s.entries
            .iter()
            .map(|e| {
                (
                    e.url.path().to_string(),
                    e.title.clone(),
                    e.transition.clone(),
                )
            })
            .collect()
    }

    /// The sequence of tests/cdp-raw `history` (entries as Chrome 153
    /// listed them): typed navigations, a pushState and a replaceState with
    /// a title change, a traversal, a reload and the reset.
    #[test]
    fn entries_as_chrome_listed_them() {
        let mut s = Session::new(false);
        commit(&mut s, &NavKind::New, "about:blank", "typed", 1);
        for (i, path) in ["a", "b", "c"].iter().enumerate() {
            commit(
                &mut s,
                &NavKind::New,
                &format!("http://h.test/{path}"),
                "typed",
                2 + i as u64,
            );
            s.sync_title(&format!("Page {}", path.to_uppercase()));
        }
        assert_eq!(s.current, 3);
        assert_eq!(s.index_at(-1), Some(2));
        assert_eq!(s.index_at(1), None);

        // Back to b and a again: a new navigation drops the forward entries.
        let b = s.entries[2].id;
        commit(&mut s, &NavKind::History(b), "http://h.test/b", "typed", 9);
        assert_eq!(s.current, 2);
        assert_eq!(
            s.entries[2].title, "",
            "the new document brings its own title"
        );
        s.sync_title("Page B");
        commit(&mut s, &NavKind::New, "http://h.test/c", "typed", 10);
        s.sync_title("Page C");
        assert_eq!(s.entries.len(), 4);

        // pushState and a title change: the new entry takes the title,
        // keeps the typed URL and has transition `link`; the entry left
        // keeps its own title.
        commit(&mut s, &NavKind::New, "http://h.test/hashpage", "typed", 11);
        s.sync_title("Hash");
        s.record(&u("http://h.test/pushed1"), None, false, "Hash");
        s.sync_title("T1");
        s.record(&u("http://h.test/pushed2"), None, false, "T1");
        s.record(&u("http://h.test/replaced2"), None, true, "T1");
        assert_eq!(
            &view(&s)[4..],
            [
                ("/hashpage".into(), "Hash".into(), "typed".into()),
                ("/pushed1".into(), "T1".into(), "link".into()),
                ("/replaced2".into(), "T1".into(), "link".into()),
            ]
        );
        assert_eq!(s.entries[5].user_typed, "http://h.test/hashpage");
        // A traversal inside the document leaves the document's title
        // alone: no entry takes it.
        s.current = 4;
        s.sync_title("T1");
        assert_eq!(s.entries[4].title, "Hash");

        // A reload keeps the position and marks the entry `reload`; the
        // reset leaves only the current entry.
        commit(
            &mut s,
            &NavKind::Reload,
            "http://h.test/hashpage",
            "reload",
            12,
        );
        assert_eq!(s.entries[4].transition, "reload");
        assert_eq!(s.entries.len(), 7);
        s.reset();
        assert_eq!((s.entries.len(), s.current), (1, 0));
        assert_eq!(s.entries[0].url.path(), "/hashpage");
    }

    #[test]
    fn same_url_replaces_and_state_stays_with_its_entry() {
        let mut s = Session::new(true);
        assert_eq!(
            (s.entries.len(), s.entries[0].url.as_str()),
            (1, "about:blank")
        );
        commit(&mut s, &NavKind::New, "http://h.test/a", "typed", 1);
        commit(&mut s, &NavKind::New, "http://h.test/a", "typed", 2);
        assert_eq!(
            s.entries.len(),
            2,
            "a navigation to the URL the page is on replaces its entry"
        );
        s.record(&u("http://h.test/p"), Some("{\"n\":1}".into()), false, "");
        s.record(&u("http://h.test/q"), Some("{\"n\":2}".into()), false, "");
        assert_eq!(
            s.current_entry().unwrap().state.as_deref(),
            Some("{\"n\":2}")
        );
        s.current = 2;
        assert_eq!(
            s.current_entry().unwrap().state.as_deref(),
            Some("{\"n\":1}")
        );
        assert_eq!(s.entries[1].state, None);
        // Entries of one document share it; a load makes a new one.
        assert_eq!(s.entries[2].doc, s.entries[1].doc);
        commit(&mut s, &NavKind::New, "http://h.test/b", "link", 3);
        assert_eq!(s.entries.len(), 4, "the forward entry (q) is gone");
        assert_eq!(s.entries[3].url.path(), "/b");
        assert_ne!(s.entries[3].doc, s.entries[2].doc);
    }
}
