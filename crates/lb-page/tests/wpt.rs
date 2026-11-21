//! WPT subset runner: serves `tests/wpt` on loopback, loads every test in
//! `MANIFEST.txt` in a page, and collects the testharness results that
//! `resources/testharnessreport.js` prints to the console.
//!
//! The gate is per file. `tests/wpt/EXPECTATIONS.txt` has one line for every
//! manifest entry (passes, total, status), written by this runner:
//!
//! ```text
//! LB_WPT_BLESS=1 cargo test --locked -p lb-page --test wpt -- --nocapture
//! ```
//!
//! A run fails when a file passes fewer subtests than expected, ends in a
//! worse status (`ok`, then `harness-error` and `harness-timeout`, then
//! `no-result`), loses all its subtests, or when the manifest and the
//! expectations name different files. Better results pass and print a hint
//! to bless. The total floor is the sum of the expected passes. See
//! `tests/README.md`.
#![cfg(feature = "chrome")]

mod common;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::{Resource, browser, context, serve_fn};

/// Time a file gets to print its result unless `EXPECTATIONS.txt` says
/// `timeout=SECS` for it.
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Time for a file that is expected to print nothing. Measured on the four
/// IndexedDB files in this state (2026-11-15): under this runner's virtual
/// clock the page thread spins at 100% CPU and prints nothing in 60 s (with
/// a real clock the same pages print in 25 to 45 ms). The page loop runs a
/// timer ahead of queued tasks only on a real clock and moves the virtual
/// clock only when no task is queued, so a page that always has a task queued
/// never runs a timer, not even testharness' own. A page that is only idle
/// prints its harness timeout at once under virtual time, so 5 s is generous.
const NO_RESULT_TIMEOUT: Duration = Duration::from_secs(5);

/// A page whose thread is still busy this long after its timeout is called
/// hung and fails the run (the polls queue behind a running script, so
/// without this a script stuck in a loop would hang the gate for ever).
const HANG_MARGIN: Duration = Duration::from_secs(180);

const BLESS_HINT: &str = "LB_WPT_BLESS=1 cargo test --locked -p lb-page --test wpt -- --nocapture";

const HEADER: &str = "\
# WPT subset expectations: one line per MANIFEST.txt entry, in manifest order:
#   <path> <passes> <total> <status> [timeout=<seconds>]  [# why]
# Written by the runner, not by hand, except a timeout= option and the # why
# of the line that has one (both are carried over):
#   LB_WPT_BLESS=1 cargo test --locked -p lb-page --test wpt -- --nocapture
# crates/lb-page/tests/wpt.rs fails when a file passes fewer subtests than
# <passes>, ends in a worse status or loses all its subtests, and when this
# file and MANIFEST.txt name different files. Better results pass and print a
# hint to bless. See tests/README.md.
#   ok               testharness finished without a harness error
#   harness-error    testharness reported an error (an uncaught exception,
#                    a failed cleanup); the subtests it saw are counted
#   harness-timeout  testharness timed out; unfinished subtests count as failed
#   no-result        the page printed no result within the file's timeout
#   timeout=<s>      how long the runner waits for the result (default 30 s,
#                    5 s for no-result)
";

/// How a file ended. `rank` orders them: `ok` is best.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Status {
    Ok,
    HarnessError,
    HarnessTimeout,
    NoResult,
}

impl Status {
    const ALL: [Status; 4] = [
        Status::Ok,
        Status::HarnessError,
        Status::HarnessTimeout,
        Status::NoResult,
    ];

    fn name(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::HarnessError => "harness-error",
            Status::HarnessTimeout => "harness-timeout",
            Status::NoResult => "no-result",
        }
    }

    fn parse(s: &str) -> Option<Status> {
        Status::ALL.into_iter().find(|v| v.name() == s)
    }

    /// 0 is best. A harness error and a harness timeout both still report
    /// the subtests, so neither is worse than the other.
    fn rank(self) -> u8 {
        match self {
            Status::Ok => 0,
            Status::HarnessError | Status::HarnessTimeout => 1,
            Status::NoResult => 2,
        }
    }

    /// testharness.js `TestsStatus`: 0 OK, 1 ERROR, 2 TIMEOUT, 3 PRECONDITION_FAILED.
    fn from_harness(code: u64) -> Status {
        match code {
            0 => Status::Ok,
            2 => Status::HarnessTimeout,
            _ => Status::HarnessError,
        }
    }
}

/// One line of `EXPECTATIONS.txt`.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Expect {
    passes: usize,
    total: usize,
    status: Status,
    /// Seconds the file gets to print its result, when not the default.
    timeout: Option<u64>,
    /// Why the status is not `ok`, for people (never compared).
    reason: String,
}

/// What a run saw for one file.
#[derive(Clone, Debug)]
struct Outcome {
    passes: usize,
    total: usize,
    status: Status,
    /// The harness message (or why nothing was printed).
    message: String,
    /// Names and messages of the subtests that did not pass (verbose runs).
    failed: Vec<String>,
    secs: f32,
}

impl Outcome {
    fn reason(&self) -> String {
        if self.status != Status::Ok {
            let why = short_reason(&self.message);
            match (why.is_empty(), self.status) {
                (false, _) => why,
                (true, Status::HarnessTimeout) => "testharness timed out".to_owned(),
                (true, _) => "testharness reported an error".to_owned(),
            }
        } else if self.total == 0 {
            "no subtests".to_owned()
        } else {
            String::new()
        }
    }
}

/// A manifest line and the URL path that loads it: `.any.js` files go
/// through a generated `.any.html` wrapper.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Entry {
    path: String,
    url_path: String,
}

fn manifest_entries(text: &str) -> Vec<Entry> {
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(|l| Entry {
            path: l.to_owned(),
            url_path: l
                .strip_suffix(".any.js")
                .map_or_else(|| l.to_owned(), |s| format!("{s}.any.html")),
        })
        .collect()
}

/// Parse `EXPECTATIONS.txt`: `path passes total status [timeout=SECS] [# why]`;
/// lines that are empty or start with `#` are comments.
fn parse_expectations(text: &str) -> Result<Vec<(String, Expect)>, String> {
    let mut out = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let (data, reason) = match raw.split_once('#') {
            Some((d, r)) => (d.trim(), r.trim()),
            None => (raw.trim(), ""),
        };
        if data.is_empty() {
            continue;
        }
        let bad = |why: &str| format!("EXPECTATIONS.txt line {}: {why}: {raw:?}", i + 1);
        let mut f = data.split_whitespace();
        let (Some(path), Some(p), Some(n), Some(st)) = (f.next(), f.next(), f.next(), f.next())
        else {
            return Err(bad("want `path passes total status`"));
        };
        let passes: usize = p.parse().map_err(|_| bad("passes is not a number"))?;
        let total: usize = n.parse().map_err(|_| bad("total is not a number"))?;
        let status = Status::parse(st).ok_or_else(|| bad("unknown status"))?;
        let mut timeout = None;
        for extra in f {
            match extra.strip_prefix("timeout=").map(str::parse::<u64>) {
                Some(Ok(secs)) if timeout.is_none() => timeout = Some(secs),
                _ => return Err(bad("unknown option (only `timeout=SECS`, once)")),
            }
        }
        if passes > total {
            return Err(bad("more passes than subtests"));
        }
        if status == Status::NoResult && total != 0 {
            return Err(bad("a no-result file has no subtests"));
        }
        let reason = reason.to_owned();
        let e = Expect {
            passes,
            total,
            status,
            timeout,
            reason,
        };
        out.push((path.to_owned(), e));
    }
    Ok(out)
}

fn render_line(path: &str, e: &Expect) -> String {
    let mut s = format!("{path} {} {} {}", e.passes, e.total, e.status.name());
    if let Some(t) = e.timeout {
        let _ = write!(s, " timeout={t}");
    }
    if !e.reason.is_empty() {
        let _ = write!(s, "  # {}", e.reason);
    }
    s
}

/// The whole file for `outcomes`. A `timeout=` option is set by hand, so it
/// comes from `old`, and so does the hand-written `# why` of such a line while
/// the file is `ok`.
fn render_expectations(
    entries: &[Entry],
    outcomes: &[Outcome],
    old: &HashMap<String, Expect>,
) -> String {
    let mut s = HEADER.to_owned();
    for (e, o) in entries.iter().zip(outcomes) {
        let prev = old.get(&e.path);
        let timeout = prev.and_then(|x| x.timeout);
        let reason = match prev {
            Some(x) if timeout.is_some() && o.status == Status::Ok && o.total > 0 => {
                x.reason.clone()
            }
            _ => o.reason(),
        };
        let x = Expect {
            passes: o.passes,
            total: o.total,
            status: o.status,
            timeout,
            reason,
        };
        s.push_str(&render_line(&e.path, &x));
        s.push('\n');
    }
    s
}

/// A harness message in one short line, without this run's port number.
fn short_reason(msg: &str) -> String {
    const ORIGIN: &str = "http://127.0.0.1:";
    let mut out = String::new();
    let mut rest = msg;
    while let Some(i) = rest.find(ORIGIN) {
        out.push_str(&rest[..i]);
        out.push_str("<origin>");
        let after = &rest[i + ORIGIN.len()..];
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        rest = &after[digits..];
    }
    out.push_str(rest);
    let one_line = out.split_whitespace().collect::<Vec<_>>().join(" ");
    let short: String = one_line.chars().take(120).collect();
    short.trim_end().to_owned()
}

/// How a run compares with its expectation.
#[derive(Debug, Default, PartialEq, Eq)]
struct Judgement {
    /// Why the gate fails.
    regressions: Vec<String>,
    /// Differences that do not fail it: better results, another total.
    notes: Vec<String>,
}

fn judge(exp: &Expect, got: &Outcome) -> Judgement {
    let mut j = Judgement::default();
    if got.passes < exp.passes {
        j.regressions
            .push(format!("passes {} -> {}", exp.passes, got.passes));
    } else if got.passes > exp.passes {
        j.notes
            .push(format!("passes {} -> {}", exp.passes, got.passes));
    }
    let change = format!("status {} -> {}", exp.status.name(), got.status.name());
    if got.status.rank() > exp.status.rank() {
        j.regressions.push(change);
    } else if got.status != exp.status {
        j.notes.push(change);
    }
    if exp.total > 0 && got.total == 0 {
        // The file would drop out of the denominator unnoticed.
        j.regressions
            .push(format!("no subtests any more (was {})", exp.total));
    } else if got.total != exp.total {
        j.notes
            .push(format!("total {} -> {}", exp.total, got.total));
    }
    j
}

/// Problems between the manifest and the expectations (empty when they name
/// the same files, each once).
fn compare_files(manifest: &[Entry], expected: &[(String, Expect)]) -> Vec<String> {
    let in_manifest: HashSet<&str> = manifest.iter().map(|e| e.path.as_str()).collect();
    let mut problems = Vec::new();
    let mut seen = HashSet::new();
    for (p, _) in expected {
        if !seen.insert(p.as_str()) {
            problems.push(format!("{p}: listed twice in EXPECTATIONS.txt"));
        } else if !in_manifest.contains(p.as_str()) {
            problems.push(format!("{p}: in EXPECTATIONS.txt but not in MANIFEST.txt"));
        }
    }
    for e in manifest {
        if !seen.contains(e.path.as_str()) {
            problems.push(format!(
                "{}: in MANIFEST.txt but not in EXPECTATIONS.txt",
                e.path
            ));
        }
    }
    problems
}

/// The manifest itself: no repeats, every file there.
fn check_manifest(base: &Path, entries: &[Entry]) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut problems = Vec::new();
    for e in entries {
        if !seen.insert(e.path.as_str()) {
            problems.push(format!("{}: listed twice in MANIFEST.txt", e.path));
        }
        if !base.join(&e.path).is_file() {
            problems.push(format!("{}: no such file under tests/wpt", e.path));
        }
    }
    problems
}

/// How long to wait for a file's result.
fn timeout_for(exp: Option<&Expect>, scale: f64) -> Duration {
    let base = match exp {
        Some(Expect {
            timeout: Some(secs),
            ..
        }) => Duration::from_secs(*secs),
        Some(Expect {
            status: Status::NoResult,
            ..
        }) => NO_RESULT_TIMEOUT,
        _ => DEFAULT_TIMEOUT,
    };
    base.mul_f64(scale)
}

fn env_flag(name: &str) -> bool {
    std::env::var(name).is_ok_and(|v| !v.is_empty() && v != "0")
}

fn group_of(path: &str) -> &str {
    path.split('/').next().unwrap_or(path)
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/wpt")
}

fn content_type(path: &str) -> &'static str {
    match path.rsplit('.').next() {
        Some("html" | "htm") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("xml") => "application/xml",
        Some("xhtml") => "application/xhtml+xml",
        Some("css") => "text/css",
        Some("json") => "application/json",
        _ => "text/plain",
    }
}

/// `.any.html` wrapper for a `.any.js` test (window scope).
fn any_wrapper(js_path: &str, src: &str) -> String {
    let mut scripts = String::new();
    for line in src.lines() {
        if let Some(s) = line.trim().strip_prefix("// META: script=") {
            scripts.push_str(&format!("<script src=\"{}\"></script>\n", s.trim()));
        }
    }
    let file = js_path.rsplit('/').next().unwrap_or(js_path);
    format!(
        "<!doctype html>\n<meta charset=utf-8>\n<script src=\"/resources/testharness.js\"></script>\n\
         <script src=\"/resources/testharnessreport.js\"></script>\n{scripts}<div id=log></div>\n<script src=\"{file}\"></script>\n"
    )
}

/// Load `url` in `page` and wait up to `timeout` for the `WPT_RESULT` line.
///
/// A script that runs for a long time keeps the page's thread busy, and the
/// poll queues behind it; the timeout is only checked between polls, so a
/// file with a long synchronous task (see `timeout=` in `EXPECTATIONS.txt`)
/// still gets the result that follows the task.
async fn run_file(page: &lb_page::Page, url: &str, timeout: Duration, verbose: bool) -> Outcome {
    let t0 = Instant::now();
    let none = |message: String| Outcome {
        passes: 0,
        total: 0,
        status: Status::NoResult,
        message,
        failed: Vec::new(),
        secs: t0.elapsed().as_secs_f32(),
    };
    if let Err(e) = page.navigate(url).await {
        return none(format!("navigation failed: {e}"));
    }
    let mut result: Option<serde_json::Value> = None;
    while t0.elapsed() < timeout {
        let msgs = page.console_messages().await.unwrap_or_default();
        if let Some(m) = msgs.iter().find(|m| m.text.starts_with("WPT_RESULT ")) {
            result = serde_json::from_str(&m.text["WPT_RESULT ".len()..]).ok();
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let Some(r) = result else {
        return none("nothing printed before the timeout".to_owned());
    };
    let subs = r["tests"].as_array().map_or(&[][..], Vec::as_slice);
    let failed = if verbose {
        subs.iter()
            .filter(|s| s["status"] != 0)
            .map(|s| format!("FAIL {}: {}", s["name"], s["message"]))
            .collect()
    } else {
        Vec::new()
    };
    Outcome {
        passes: subs.iter().filter(|s| s["status"] == 0).count(),
        total: subs.len(),
        status: Status::from_harness(r["status"].as_u64().unwrap_or(1)),
        message: r["message"].as_str().unwrap_or("").to_owned(),
        failed,
        secs: t0.elapsed().as_secs_f32(),
    }
}

/// The committed expectations; `None` when the file does not exist.
fn read_expectations(path: &Path) -> Option<Vec<(String, Expect)>> {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => panic!("cannot read {}: {e}", path.display()),
    };
    Some(parse_expectations(&text).unwrap_or_else(|e| panic!("{e}")))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn wpt_subset() {
    let bless = env_flag("LB_WPT_BLESS");
    let verbose = std::env::var_os("WPT_VERBOSE").is_some();
    let filter = std::env::var("WPT_FILTER").ok();
    let scale: f64 = std::env::var("WPT_TIMEOUT_SCALE")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);
    assert!(
        !(bless && filter.is_some()),
        "LB_WPT_BLESS writes the whole file: unset WPT_FILTER"
    );

    let base = root();
    let manifest = manifest_entries(&std::fs::read_to_string(base.join("MANIFEST.txt")).unwrap());
    let problems = check_manifest(&base, &manifest);
    assert!(
        problems.is_empty(),
        "MANIFEST.txt:\n  {}",
        problems.join("\n  ")
    );

    // The expectations must name exactly the manifest's files, before
    // anything runs.
    let expect_path = base.join("EXPECTATIONS.txt");
    let old_list = read_expectations(&expect_path);
    if !bless {
        let Some(list) = &old_list else {
            panic!(
                "{} is missing; write it with:\n  {BLESS_HINT}",
                expect_path.display()
            );
        };
        let problems = compare_files(&manifest, list);
        assert!(
            problems.is_empty(),
            "MANIFEST.txt and EXPECTATIONS.txt differ:\n  {}\nbless after checking the change:\n  {BLESS_HINT}",
            problems.join("\n  ")
        );
    }
    let old: HashMap<String, Expect> = old_list.unwrap_or_default().into_iter().collect();

    let base2 = base.clone();
    let server = serve_fn(Arc::new(move |path: &str| {
        let rel = path.trim_start_matches('/');
        if let Some(js) = rel.strip_suffix(".any.html") {
            let js = format!("{js}.any.js");
            let src = std::fs::read_to_string(base2.join(&js)).ok()?;
            return Some(Resource::html(&any_wrapper(&js, &src)));
        }
        if rel.contains("..") {
            return None;
        }
        let bytes = std::fs::read(base2.join(rel)).ok()?;
        Some(Resource::new(content_type(rel), bytes))
    }))
    .await;

    let selected: Vec<&Entry> = manifest
        .iter()
        .filter(|e| {
            filter
                .as_ref()
                .is_none_or(|f| e.url_path.contains(f) || e.path.contains(f))
        })
        .collect();
    // A filter that matches nothing would pass with nothing checked.
    assert!(
        !selected.is_empty(),
        "WPT_FILTER={} matches no manifest entry",
        filter.as_deref().unwrap_or("")
    );

    let b = browser();
    let ctx = context(true);
    let mut outcomes: Vec<Outcome> = Vec::new();
    for e in &selected {
        if verbose {
            eprintln!("running {}", e.url_path);
        }
        let timeout = timeout_for(old.get(&e.path), scale);
        let url = server.url(&format!("/{}", e.url_path));
        let run = async {
            let page = b.new_page(&ctx).await.unwrap();
            let o = run_file(&page, &url, timeout, verbose).await;
            let _ = page.close().await;
            o
        };
        let o = tokio::time::timeout(timeout + HANG_MARGIN, run)
            .await
            .unwrap_or_else(|_| {
                panic!(
                    "{}: the page thread was still busy {} s after the file's {} s timeout; \
                     a script is stuck",
                    e.path,
                    HANG_MARGIN.as_secs(),
                    timeout.as_secs()
                )
            });
        let note = match o.status {
            Status::Ok => String::new(),
            Status::NoResult => " (no result)".to_owned(),
            Status::HarnessError => format!(" (harness error: {})", o.message),
            Status::HarnessTimeout => " (harness timeout)".to_owned(),
        };
        let slow = if o.secs >= 5.0 {
            format!(" [{:.0} s]", o.secs)
        } else {
            String::new()
        };
        println!("{:4}/{:<4} {}{note}{slow}", o.passes, o.total, e.url_path);
        for f in &o.failed {
            println!("          {f}");
        }
        outcomes.push(o);
    }

    report(&selected, &outcomes);

    if bless {
        let all: Vec<Entry> = selected.iter().map(|e| (*e).clone()).collect();
        let text = render_expectations(&all, &outcomes, &old);
        std::fs::write(&expect_path, &text).unwrap();
        let (regressions, notes) = judgements(&selected, &outcomes, &old);
        for r in &regressions {
            println!("blessed a regression: {r}");
        }
        println!(
            "blessed {}: {} files ({} worse than before, {} better or changed)",
            expect_path.display(),
            all.len(),
            regressions.len(),
            notes.len()
        );
        return;
    }

    let (regressions, notes) = judgements(&selected, &outcomes, &old);
    // The total floor is the sum of what the expectations say: each file's
    // own check already implies it, so this only fails together with them.
    let floor: usize = selected.iter().map(|e| old[&e.path].passes).sum();
    let pass: usize = outcomes.iter().map(|o| o.passes).sum();
    println!("expected floor (sum of EXPECTATIONS.txt passes): {floor}, run: {pass}");
    if !notes.is_empty() {
        println!(
            "{} file(s) are better than, or differ from, tests/wpt/EXPECTATIONS.txt:",
            notes.len()
        );
        for n in &notes {
            println!("  {n}");
        }
        println!("if that is intended, bless it and commit the file:\n  {BLESS_HINT}");
    }
    assert!(
        regressions.is_empty(),
        "WPT regressions against tests/wpt/EXPECTATIONS.txt ({} file(s)):\n  {}\n\
         if they are intended, bless them and commit the file:\n  {BLESS_HINT}",
        regressions.len(),
        regressions.join("\n  ")
    );
    assert!(pass >= floor, "WPT passes dropped: {pass} < {floor}");
}

/// (regressions, notes) as report lines, one per file that differs.
fn judgements(
    selected: &[&Entry],
    outcomes: &[Outcome],
    old: &HashMap<String, Expect>,
) -> (Vec<String>, Vec<String>) {
    let (mut regressions, mut notes) = (Vec::new(), Vec::new());
    for (e, o) in selected.iter().zip(outcomes) {
        let Some(x) = old.get(&e.path) else { continue };
        let j = judge(x, o);
        if !j.regressions.is_empty() {
            regressions.push(format!("{}: {}", e.path, j.regressions.join(", ")));
        }
        if !j.notes.is_empty() {
            notes.push(format!("{}: {}", e.path, j.notes.join(", ")));
        }
    }
    (regressions, notes)
}

/// Totals, the groups (first path component), and the files that have no
/// subtests or a harness problem, listed one by one.
fn report(selected: &[&Entry], outcomes: &[Outcome]) {
    let pass: usize = outcomes.iter().map(|o| o.passes).sum();
    let total: usize = outcomes.iter().map(|o| o.total).sum();
    let files_ok = outcomes
        .iter()
        .filter(|o| o.total > 0 && o.passes == o.total && o.status == Status::Ok)
        .count();
    let problems = outcomes.iter().filter(|o| o.status != Status::Ok).count();
    println!(
        "WPT subset: {pass}/{total} subtests pass, {files_ok}/{} files fully pass, {problems} harness errors/timeouts",
        outcomes.len()
    );

    struct Group<'a> {
        name: &'a str,
        passes: usize,
        total: usize,
        files: usize,
        problems: usize,
    }
    let mut groups: Vec<Group> = Vec::new();
    for (e, o) in selected.iter().zip(outcomes) {
        let name = group_of(&e.path);
        let i = groups
            .iter()
            .position(|g| g.name == name)
            .unwrap_or_else(|| {
                groups.push(Group {
                    name,
                    passes: 0,
                    total: 0,
                    files: 0,
                    problems: 0,
                });
                groups.len() - 1
            });
        let g = &mut groups[i];
        g.passes += o.passes;
        g.total += o.total;
        g.files += 1;
        g.problems += usize::from(o.status != Status::Ok);
    }
    println!(
        "{:<16} {:>12} {:>6} {:>9}",
        "group", "passes/total", "files", "problems"
    );
    for g in &groups {
        let counts = format!("{}/{}", g.passes, g.total);
        println!(
            "{:<16} {counts:>12} {:>6} {:>9}",
            g.name, g.files, g.problems
        );
    }

    let rows: Vec<(&Entry, &Outcome)> = selected.iter().copied().zip(outcomes).collect();
    let empty: Vec<_> = rows.iter().filter(|(_, o)| o.total == 0).collect();
    println!("files with no subtests (0/0): {}", empty.len());
    for (e, o) in &empty {
        println!("  {:<15} {}  # {}", o.status.name(), e.path, o.reason());
    }
    let bad: Vec<_> = rows
        .iter()
        .filter(|(_, o)| o.status != Status::Ok)
        .collect();
    println!(
        "files with a harness error, a harness timeout or no result: {}",
        bad.len()
    );
    for (e, o) in &bad {
        println!(
            "  {:<15} {:>4}/{:<4} {}  # {}",
            o.status.name(),
            o.passes,
            o.total,
            e.path,
            o.reason()
        );
    }
}

// The gate's own logic: no browser.

fn exp(passes: usize, total: usize, status: Status) -> Expect {
    Expect {
        passes,
        total,
        status,
        timeout: None,
        reason: String::new(),
    }
}

fn got(passes: usize, total: usize, status: Status) -> Outcome {
    Outcome {
        passes,
        total,
        status,
        message: String::new(),
        failed: Vec::new(),
        secs: 0.0,
    }
}

/// The committed file names exactly the manifest's files, so a new or removed
/// test cannot go unnoticed even when the long run is not done.
#[test]
fn expectations_file_matches_the_manifest() {
    if env_flag("LB_WPT_BLESS") {
        return; // the file is being written by the run of `wpt_subset`
    }
    let base = root();
    let manifest = manifest_entries(&std::fs::read_to_string(base.join("MANIFEST.txt")).unwrap());
    let problems = check_manifest(&base, &manifest);
    assert!(
        problems.is_empty(),
        "MANIFEST.txt:\n  {}",
        problems.join("\n  ")
    );
    let Some(list) = read_expectations(&base.join("EXPECTATIONS.txt")) else {
        panic!("tests/wpt/EXPECTATIONS.txt is missing; write it with:\n  {BLESS_HINT}");
    };
    let problems = compare_files(&manifest, &list);
    assert!(
        problems.is_empty(),
        "MANIFEST.txt and EXPECTATIONS.txt differ:\n  {}\nbless after checking the change:\n  {BLESS_HINT}",
        problems.join("\n  ")
    );
}

#[test]
fn fewer_passes_or_a_worse_status_fail() {
    let e = exp(10, 12, Status::Ok);
    assert_eq!(judge(&e, &got(10, 12, Status::Ok)), Judgement::default());
    assert_eq!(
        judge(&e, &got(9, 12, Status::Ok)).regressions,
        ["passes 10 -> 9"]
    );
    assert_eq!(
        judge(&e, &got(10, 12, Status::HarnessError)).regressions,
        ["status ok -> harness-error"]
    );
    // Nothing printed: fewer passes, a worse status and no subtests left.
    assert_eq!(judge(&e, &got(0, 0, Status::NoResult)).regressions.len(), 3);
    // harness-error -> no-result is worse as well.
    let bad = exp(2, 4, Status::HarnessError);
    assert!(
        !judge(&bad, &got(2, 4, Status::NoResult))
            .regressions
            .is_empty()
    );
}

#[test]
fn better_results_pass_with_a_note() {
    let e = exp(10, 12, Status::HarnessError);
    let j = judge(&e, &got(12, 12, Status::Ok));
    assert!(j.regressions.is_empty());
    assert_eq!(j.notes, ["passes 10 -> 12", "status harness-error -> ok"]);
    // An error and a timeout are the same level.
    let j = judge(&e, &got(10, 12, Status::HarnessTimeout));
    assert!(j.regressions.is_empty());
    assert_eq!(j.notes, ["status harness-error -> harness-timeout"]);
    // A file that registered no subtests now registers some.
    let j = judge(
        &exp(0, 0, Status::HarnessError),
        &got(2, 3, Status::HarnessError),
    );
    assert!(j.regressions.is_empty());
    assert_eq!(j.notes, ["passes 0 -> 2", "total 0 -> 3"]);
    // no-result -> harness-error is better too.
    let j = judge(
        &exp(0, 0, Status::NoResult),
        &got(0, 0, Status::HarnessError),
    );
    assert!(j.regressions.is_empty());
}

#[test]
fn a_file_may_not_drop_out_of_the_denominator() {
    // 0 of 5 -> 0 of 0 would improve the ratio without a single fix.
    let j = judge(&exp(0, 5, Status::Ok), &got(0, 0, Status::Ok));
    assert_eq!(j.regressions, ["no subtests any more (was 5)"]);
    // A smaller total with the same passes is only noted.
    let j = judge(&exp(3, 5, Status::Ok), &got(3, 4, Status::Ok));
    assert!(j.regressions.is_empty());
    assert_eq!(j.notes, ["total 5 -> 4"]);
}

#[test]
fn expectations_round_trip() {
    let text = "# comment\n\ndom/a.html 3 4 ok\ndom/b.any.js 0 0 no-result  # nothing printed\n\
                x/c.html 1 1 ok timeout=300  # slow\n";
    let list = parse_expectations(text).unwrap();
    assert_eq!(list.len(), 3);
    assert_eq!(list[1].1.status, Status::NoResult);
    assert_eq!(list[1].1.reason, "nothing printed");
    assert_eq!(list[2].1.timeout, Some(300));
    let lines: Vec<String> = list.iter().map(|(p, e)| render_line(p, e)).collect();
    assert_eq!(
        lines,
        [
            "dom/a.html 3 4 ok",
            "dom/b.any.js 0 0 no-result  # nothing printed",
            "x/c.html 1 1 ok timeout=300  # slow"
        ]
    );
}

#[test]
fn bad_expectation_lines_are_refused() {
    for bad in [
        "a.html 1 ok",
        "a.html x 2 ok",
        "a.html 3 2 ok",
        "a.html 0 0 fine",
        "a.html 0 0 ok timeout=x",
        "a.html 0 0 ok colour=red",
        "a.html 0 0 ok timeout=5 timeout=6",
        "a.html 0 1 no-result",
    ] {
        assert!(parse_expectations(bad).is_err(), "{bad}");
    }
}

#[test]
fn manifest_and_expectations_must_name_the_same_files() {
    let manifest = manifest_entries("# comment\na.html\nb/c.any.js\n");
    assert_eq!(manifest[1].url_path, "b/c.any.html");
    let parse = |t| parse_expectations(t).unwrap();
    let same = parse("a.html 1 1 ok\nb/c.any.js 1 1 ok\n");
    assert!(compare_files(&manifest, &same).is_empty());
    assert_eq!(
        compare_files(&manifest, &parse("a.html 1 1 ok\n")),
        ["b/c.any.js: in MANIFEST.txt but not in EXPECTATIONS.txt"]
    );
    let extra = parse("a.html 1 1 ok\nb/c.any.js 1 1 ok\nd.html 0 0 ok\n");
    assert_eq!(
        compare_files(&manifest, &extra),
        ["d.html: in EXPECTATIONS.txt but not in MANIFEST.txt"]
    );
    let twice = parse("a.html 1 1 ok\na.html 1 1 ok\nb/c.any.js 1 1 ok\n");
    assert_eq!(
        compare_files(&manifest, &twice),
        ["a.html: listed twice in EXPECTATIONS.txt"]
    );
}

#[test]
fn files_expected_to_print_nothing_get_a_short_timeout() {
    let none = exp(0, 0, Status::NoResult);
    assert_eq!(timeout_for(Some(&none), 1.0), NO_RESULT_TIMEOUT);
    assert_eq!(
        timeout_for(Some(&exp(1, 1, Status::Ok)), 1.0),
        DEFAULT_TIMEOUT
    );
    assert_eq!(timeout_for(None, 1.0), DEFAULT_TIMEOUT);
    let slow = Expect {
        timeout: Some(300),
        ..exp(1, 1, Status::Ok)
    };
    assert_eq!(timeout_for(Some(&slow), 1.0), Duration::from_secs(300));
    assert_eq!(timeout_for(Some(&slow), 2.0), Duration::from_secs(600));
    // An explicit timeout wins for a no-result file too.
    let longer = Expect {
        timeout: Some(60),
        ..none
    };
    assert_eq!(timeout_for(Some(&longer), 1.0), Duration::from_secs(60));
}

#[test]
fn reasons_are_one_short_line_without_the_port() {
    assert_eq!(
        short_reason("Uncaught  at http://127.0.0.1:39395/dom/x.html:217:7\n  more"),
        "Uncaught at <origin>/dom/x.html:217:7 more"
    );
    assert!(short_reason(&"x".repeat(500)).chars().count() <= 120);
    // A cut that lands on a space leaves no trailing space.
    let cut = format!("{} tail", "x".repeat(119));
    assert_eq!(short_reason(&cut), "x".repeat(119));
}

#[test]
fn bless_keeps_timeouts_and_writes_reasons() {
    let entries = manifest_entries("a.html\nb.any.js\nc.html\n");
    let outcomes = vec![
        got(1, 1, Status::Ok),
        Outcome {
            message: "Uncaught at http://127.0.0.1:1234/x".into(),
            ..got(0, 0, Status::HarnessError)
        },
        got(0, 0, Status::Ok),
    ];
    let old = HashMap::from([(
        "a.html".to_owned(),
        Expect {
            timeout: Some(300),
            reason: "one long task".to_owned(),
            ..exp(0, 0, Status::NoResult)
        },
    )]);
    let text = render_expectations(&entries, &outcomes, &old);
    let lines: Vec<&str> = text.lines().filter(|l| !l.starts_with('#')).collect();
    assert_eq!(
        lines,
        [
            "a.html 1 1 ok timeout=300  # one long task",
            "b.any.js 0 0 harness-error  # Uncaught at <origin>/x",
            "c.html 0 0 ok  # no subtests"
        ]
    );
    // What was written is what the gate reads back.
    let back = parse_expectations(&text).unwrap();
    assert_eq!(back.len(), 3);
    assert_eq!(back[0].1.timeout, Some(300));
}
