//! Running `serve` and `mcp` as services: the panic hook, `--log`, the
//! SIGINT/SIGTERM handler and `--version --verbose`. No extra crates:
//! `libc` for signals and a few lines of `tracing` subscriber.

use std::fmt::Write as _;
use std::future::Future;
use std::io::Write as _;
use std::time::Duration;

/// How long `serve` and `mcp` may take to stop after the first SIGINT or
/// SIGTERM before they exit anyway (status 130): a page stuck in a script
/// would otherwise keep its worker thread, and so the process, alive.
pub const SHUTDOWN_DEADLINE: Duration = Duration::from_secs(5);

/// The commit the binary was built from (`build.rs`), or `unknown`.
pub const COMMIT: &str = env!("LB_GIT_COMMIT");
/// `true` when the work tree had uncommitted changes to tracked files at
/// build time (as of the build script's last run).
const DIRTY: &str = env!("LB_GIT_DIRTY");

fn family() -> &'static str {
    lb_js::COMPILED_FAMILY.map_or("none", |f| f.as_str())
}

/// `--version`: the first line is fixed (the SDK, bench and tests parse
/// it); `--verbose` adds the commit on a second line.
pub fn print_version(verbose: bool) {
    println!("lightbrowser {} ({})", env!("CARGO_PKG_VERSION"), family());
    if verbose {
        let dirty = if DIRTY == "true" { " (dirty)" } else { "" };
        println!("commit {COMMIT}{dirty}");
    }
}

/// Prints one line naming the build, thread, location and message for any
/// panic. Release builds abort right after (`panic = "abort"`); debug and
/// test builds may still catch it (a host function's panic becomes a JS
/// error there). `RUST_BACKTRACE` adds a backtrace as before.
pub fn install_panic_hook() {
    std::panic::set_hook(Box::new(|info| {
        let thread = std::thread::current();
        let name = thread.name().unwrap_or("<unnamed>");
        let at = info
            .location()
            .map_or_else(|| "unknown location".to_string(), |l| l.to_string());
        let payload = info.payload();
        let msg = payload
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| payload.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("Box<dyn Any>");
        let msg = msg.replace('\n', " | ");
        // Never `eprintln!` here: a failed write (stderr closed) would
        // panic inside the hook and abort.
        let mut err = std::io::stderr().lock();
        let _ = writeln!(
            err,
            "lightbrowser {} ({}, commit {COMMIT}): panic in thread '{name}' at {at}: {msg}",
            env!("CARGO_PKG_VERSION"),
            family(),
        );
        let bt = std::backtrace::Backtrace::capture();
        if bt.status() == std::backtrace::BacktraceStatus::Captured {
            let _ = writeln!(err, "{bt}");
        }
    }));
}

/// Parses a `--log` level.
pub fn parse_log_level(v: &str) -> Result<tracing::Level, String> {
    match v {
        "error" => Ok(tracing::Level::ERROR),
        "warn" => Ok(tracing::Level::WARN),
        "info" => Ok(tracing::Level::INFO),
        "debug" => Ok(tracing::Level::DEBUG),
        other => Err(format!(
            "--log must be one of error, warn, info, debug, got {other:?}"
        )),
    }
}

/// `--log LEVEL`: `tracing` events at `level` and above go to stderr, one
/// line each. Without it no subscriber is set and events cost nothing.
pub fn init_log(level: Option<tracing::Level>) {
    if let Some(max) = level {
        let _ = tracing::subscriber::set_global_default(StderrLog { max });
    }
}

struct StderrLog {
    max: tracing::Level,
}

/// Collects an event's fields: the message, then `name=value` pairs.
struct Fields(String);

impl tracing::field::Visit for Fields {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            let _ = write!(self.0, "{value:?}");
        } else {
            let _ = write!(self.0, " {}={value:?}", field.name());
        }
    }
}

impl tracing::Subscriber for StderrLog {
    fn enabled(&self, m: &tracing::Metadata<'_>) -> bool {
        // More verbose levels compare greater.
        *m.level() <= self.max
    }

    fn max_level_hint(&self) -> Option<tracing::level_filters::LevelFilter> {
        Some(tracing::level_filters::LevelFilter::from_level(self.max))
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        // Spans are not shown; every span gets the same id.
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, e: &tracing::Event<'_>) {
        let mut f = Fields(String::new());
        e.record(&mut f);
        let m = e.metadata();
        let thread = std::thread::current();
        // A closed stderr must not panic (and abort) the browser.
        let _ = writeln!(
            std::io::stderr().lock(),
            "{} {} [{}] {}",
            m.level(),
            m.target(),
            thread.name().unwrap_or("-"),
            f.0
        );
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

#[cfg(unix)]
mod sig {
    //! Self-pipe: the handler writes one byte to a non-blocking pipe; a
    //! thread blocked in `read` on the other end does the rest. Process
    //! control only, not fingerprint data.
    use std::sync::atomic::{AtomicBool, AtomicI32, Ordering};

    /// Set by the first SIGINT or SIGTERM.
    static FIRED: AtomicBool = AtomicBool::new(false);
    /// Write end of the pipe, -1 before `install`.
    static PIPE_W: AtomicI32 = AtomicI32::new(-1);

    extern "C" fn on_signal(_: libc::c_int) {
        if FIRED.swap(true, Ordering::SeqCst) {
            // A second signal: the graceful stop is stuck (or the user is
            // in a hurry).
            // SAFETY: `_exit` is async-signal-safe and ends the process.
            unsafe { libc::_exit(130) }
        }
        let fd = PIPE_W.load(Ordering::SeqCst);
        if fd >= 0 {
            let b = 1u8;
            // SAFETY: `write` is async-signal-safe; one byte from a local
            // into a pipe we own. errno is saved and restored around it.
            unsafe {
                let e = errno();
                libc::write(fd, (&b as *const u8).cast(), 1);
                *e.0 = e.1;
            }
        }
    }

    /// The errno location and its value.
    unsafe fn errno() -> (*mut libc::c_int, libc::c_int) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        // SAFETY: returns this thread's errno slot.
        let p = unsafe { libc::__errno_location() };
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        // SAFETY: as above (Darwin and the BSDs).
        let p = unsafe { libc::__error() };
        // SAFETY: `p` is this thread's valid errno slot.
        (p, unsafe { *p })
    }

    /// Makes the pipe and installs the handlers; returns the read end.
    pub fn install() -> Option<libc::c_int> {
        let mut fds = [0 as libc::c_int; 2];
        // SAFETY: plain pipe/fcntl/signal calls on descriptors we own; the
        // handler only does async-signal-safe work (see `on_signal`).
        unsafe {
            if libc::pipe(fds.as_mut_ptr()) != 0 {
                return None;
            }
            for fd in fds {
                libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
            }
            let fl = libc::fcntl(fds[1], libc::F_GETFL);
            libc::fcntl(fds[1], libc::F_SETFL, fl | libc::O_NONBLOCK);
            PIPE_W.store(fds[1], Ordering::SeqCst);
            libc::signal(libc::SIGINT, on_signal as *const () as libc::sighandler_t);
            libc::signal(libc::SIGTERM, on_signal as *const () as libc::sighandler_t);
        }
        Some(fds[0])
    }

    /// Blocks until the handler wrote its byte. False if the pipe failed.
    pub fn wait(fd: libc::c_int) -> bool {
        let mut b = 0u8;
        loop {
            // SAFETY: reads one byte into a local from a pipe we own.
            let n = unsafe { libc::read(fd, (&mut b as *mut u8).cast(), 1) };
            if n == 1 {
                return true;
            }
            if n < 0 && std::io::Error::last_os_error().kind() == std::io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
    }
}

/// Installs the SIGINT/SIGTERM handler now and returns a future that
/// completes on the first signal. A second signal exits at once, and if
/// the process is still alive [`SHUTDOWN_DEADLINE`] after the first it
/// exits anyway; both with status 130. Nothing wakes while no signal
/// comes: one thread sleeps in `read` on a self-pipe.
pub fn stop_signal() -> impl Future<Output = ()> {
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    #[cfg(unix)]
    if let Some(fd) = sig::install() {
        let started = std::thread::Builder::new()
            .name("lb-signal".into())
            .spawn(move || {
                if !sig::wait(fd) {
                    return;
                }
                let _ = tx.send(());
                std::thread::sleep(SHUTDOWN_DEADLINE);
                let _ = writeln!(
                    std::io::stderr().lock(),
                    "lightbrowser: still running {} s after the stop signal; exiting now",
                    SHUTDOWN_DEADLINE.as_secs()
                );
                // SAFETY: ends the process; the graceful stop is stuck.
                unsafe { libc::_exit(130) }
            });
        if started.is_err() {
            // SAFETY: restores the default actions; nobody reads the pipe.
            unsafe {
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::signal(libc::SIGTERM, libc::SIG_DFL);
            }
            let _ = writeln!(
                std::io::stderr().lock(),
                "lightbrowser: no signal thread; SIGINT/SIGTERM stop the process at once"
            );
        }
    }
    #[cfg(not(unix))]
    drop(tx);
    async move {
        if rx.await.is_err() {
            // No handler: never stop from here.
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_levels() {
        assert_eq!(parse_log_level("debug"), Ok(tracing::Level::DEBUG));
        assert_eq!(parse_log_level("error"), Ok(tracing::Level::ERROR));
        assert!(parse_log_level("trace").is_err());
        assert!(parse_log_level("").is_err());
        let s = StderrLog {
            max: tracing::Level::WARN,
        };
        assert!(tracing::Level::ERROR <= s.max);
        assert!(tracing::Level::INFO > s.max);
    }
}
