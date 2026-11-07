//! ICU data for V8, shipped compressed (M6 binary size, ADR 0013).
//!
//! The prebuilt `v8` static library embeds ICU's common data package
//! (10.9 MB, symbol `icudt78_dat` in its `icudtl_dat.o`). ICU's loader
//! (`udata.o`) references that symbol, so the linker pulls the archive
//! member into every binary that links V8. We define `icudt78_dat` here
//! instead, as a valid ICU package with no items: the symbol is then
//! already defined when the linker reads the archive, its copy is never
//! extracted (a second definition would be a duplicate-symbol error, so this
//! cannot fail silently), and the binary carries the same bytes
//! zstd-compressed (`icu/icudt78.dat.zst`, made by the `icu_pack` example).
//!
//! The data ships as independent zstd frames of [`CHUNK`] bytes each
//! (`icu/icudt78.dat.zch`, see [`pack`]). `install` registers it with
//! `udata_setCommonData` before V8 is initialised. ICU searches registered
//! packages in order, so every item comes from that copy; the empty
//! linked-in package is only consulted for items that exist in neither,
//! exactly as the full one was for items it lacked. The data is
//! byte-identical to the archive's (`icu_data` tests in `tests/v8.rs`), so
//! Intl behaviour is unchanged.
//!
//! On Linux and macOS the 10.9 MB package is reserved as inaccessible
//! memory and each page of it is decompressed the first time ICU touches
//! it (see `lazy`), like a file mapping that pages in on demand: a page
//! load reads about 120 KB of it (the table of contents and a few items
//! such as the root locale and time zone data), so about 27 chunks of
//! 4 KiB (0.1 MB, < 1 ms; on Apple silicon's 16 KiB pages about 0.3 MB)
//! become resident instead of all of it (10.9 MB, ~15 ms). If the
//! reservation or the fault handler cannot be set up, if the page size
//! and [`CHUNK`] have no common unit, or on other systems, the whole
//! package is decompressed into the heap at once. `LB_ICU_EAGER=1` asks for that
//! too: valgrind's callgrind does not retry a faulting load correctly after
//! the handler filled the page in, so `tools/hot_symbols` sets it.

use std::sync::OnceLock;

/// Name of the data symbol in the prebuilt archive (ICU major version 78,
/// matching `v8::icu::set_common_data_78`).
pub const SYMBOL: &str = "icudt78_dat";

/// Archive member that holds the data.
pub const MEMBER: &str = "icudtl_dat.o";

/// The archive's ICU data in chunks (see [`pack`]).
pub const PACKED: &[u8] = include_bytes!("../../icu/icudt78.dat.zch");

/// Bytes of ICU data per compressed frame: one 4 KiB page, so only the
/// pages ICU reads become resident. Smaller chunks compress worse: 4 KiB
/// frames take 4.92 MB, 0.66 MB more than 128 KiB frames (4.26 MB; 16 KiB:
/// 4.60 MB), and a typical page keeps ~0.1 MB of the package resident
/// instead of ~0.6 MB (ADR 0013, revision 4).
pub const CHUNK: usize = 4 * 1024;

const MAGIC: &[u8; 8] = b"LBICUZC1";

/// Compress `data` for [`PACKED`]: magic, the data length, [`CHUNK`] and
/// the chunk count (u32 LE each), the end offset of every frame relative
/// to the first (u32 LE each), then one zstd frame (level 22, with content
/// size) per chunk. Used by the `icu_pack` example.
pub fn pack(data: &[u8]) -> Vec<u8> {
    let frames: Vec<Vec<u8>> = data
        .chunks(CHUNK)
        .map(|c| {
            let mut z = zstd::bulk::Compressor::new(22).expect("zstd compressor");
            z.include_contentsize(true).expect("content size");
            z.compress(c).expect("zstd compress")
        })
        .collect();
    let mut out = Vec::new();
    out.extend_from_slice(MAGIC);
    for v in [data.len(), CHUNK, frames.len()] {
        out.extend_from_slice(&(v as u32).to_le_bytes());
    }
    let mut end = 0usize;
    for f in &frames {
        end += f.len();
        out.extend_from_slice(&(end as u32).to_le_bytes());
    }
    for f in &frames {
        out.extend_from_slice(f);
    }
    out
}

/// The parsed header of [`PACKED`].
#[derive(Clone, Copy)]
struct Packed {
    len: usize,
    count: usize,
    /// Frame end offsets (u32 LE each).
    ends: &'static [u8],
    frames: &'static [u8],
}

impl Packed {
    fn get() -> Packed {
        let u32_at = |at: usize| {
            u32::from_le_bytes(PACKED[at..at + 4].try_into().expect("4 bytes")) as usize
        };
        assert_eq!(&PACKED[..8], MAGIC, "icu/icudt78.dat.zch: bad magic");
        let (len, chunk, count) = (u32_at(8), u32_at(12), u32_at(16));
        assert_eq!(chunk, CHUNK, "icu/icudt78.dat.zch: chunk size");
        assert_eq!(
            count,
            len.div_ceil(CHUNK),
            "icu/icudt78.dat.zch: chunk count"
        );
        let ends = &PACKED[20..20 + 4 * count];
        let frames = &PACKED[20 + 4 * count..];
        Packed {
            len,
            count,
            ends,
            frames,
        }
    }

    /// The compressed frame of chunk `i`.
    fn frame(&self, i: usize) -> &'static [u8] {
        let end_at = |j: usize| {
            u32::from_le_bytes(self.ends[4 * j..4 * j + 4].try_into().expect("4 bytes")) as usize
        };
        let start = if i == 0 { 0 } else { end_at(i - 1) };
        &self.frames[start..end_at(i)]
    }

    /// Bytes of data in chunk `i` (the last one is short).
    fn chunk_len(&self, i: usize) -> usize {
        CHUNK.min(self.len - i * CHUNK)
    }
}

/// An ICU common-data package ("CmnD", format 1) with an empty table of
/// contents: a 32-byte header (`MappedData` plus `UDataInfo`, little
/// endian, ASCII, 2-byte UChar) followed by an entry count of 0.
#[repr(C, align(16))]
pub struct EmptyPackage([u8; 48]);

const fn empty_package() -> EmptyPackage {
    let mut b = [0u8; 48];
    // MappedData: headerSize = 32, magic 0xda 0x27.
    b[0] = 32;
    b[2] = 0xda;
    b[3] = 0x27;
    // UDataInfo: size = 20, reservedWord, isBigEndian = 0,
    // charsetFamily = 0 (ASCII), sizeofUChar = 2, reservedByte.
    b[4] = 20;
    b[10] = 2;
    // dataFormat "CmnD", formatVersion 1.0.0.0, dataVersion 3.0.0.0 (as in
    // the real package).
    b[12] = b'C';
    b[13] = b'm';
    b[14] = b'n';
    b[15] = b'D';
    b[16] = 1;
    b[20] = 3;
    // b[32..36]: TOC entry count = 0.
    EmptyPackage(b)
}

/// Replaces the archive's `icudt78_dat` (see the module docs). Never read
/// by Rust; ICU's loader reads it when an item is missing from the
/// installed package.
#[unsafe(no_mangle)]
#[used]
#[allow(non_upper_case_globals)]
pub static icudt78_dat: EmptyPackage = empty_package();

/// The bytes of [`icudt78_dat`].
pub fn empty_package_bytes() -> &'static [u8] {
    &icudt78_dat.0
}

static DATA: OnceLock<&'static [u8]> = OnceLock::new();

/// The ICU data, for the life of the process (as ICU requires). 16-byte
/// aligned. On Linux and macOS its pages are decompressed when first read
/// (see the module docs); elsewhere, or if that cannot be set up, all at
/// once here.
pub fn data() -> &'static [u8] {
    DATA.get_or_init(|| {
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        if std::env::var_os("LB_ICU_EAGER").is_none()
            && let Some(d) = lazy::map(Packed::get())
        {
            return d;
        }
        let words: &'static [u128] = Box::leak(decompress_all());
        // SAFETY: the leaked allocation holds at least `len` initialised
        // bytes, lives for the process, and u8 has no alignment or validity
        // requirements.
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), Packed::get().len) }
    })
}

/// Drops the pages of [`PACKED`] that reading compressed frames mapped
/// (the kernel maps up to 64 KB of the binary around each fault) from this
/// process's RSS: `madvise(MADV_DONTNEED)` on the whole pages inside it.
/// They are clean pages of the read-only executable mapping, so this only
/// unmaps them; a later read maps them again from the page cache.
/// Async-signal-safe (one system call), so the lazy fault handler calls it
/// after each chunk.
fn release_packed() {
    #[cfg(target_os = "linux")]
    {
        let start = PACKED.as_ptr() as usize;
        let first = start.next_multiple_of(4096);
        let last = (start + PACKED.len()) & !4095;
        if last > first {
            // SAFETY: whole pages inside the immutable `PACKED` static;
            // MADV_DONTNEED on a private file mapping that was never
            // written leaves its contents unchanged.
            unsafe {
                libc::madvise(
                    first as *mut libc::c_void,
                    last - first,
                    libc::MADV_DONTNEED,
                );
            }
        }
    }
}

/// The whole package decompressed into a new heap buffer (u128s: ICU
/// wants 16-byte alignment). The eager path of [`data`]; tests compare
/// the lazy one against it.
pub fn decompress_all() -> Box<[u128]> {
    let p = Packed::get();
    let mut words = vec![0u128; p.len.div_ceil(16)].into_boxed_slice();
    // SAFETY: `words` owns `len.div_ceil(16) * 16 >= len` initialised
    // bytes, and u8 has no alignment or validity requirements.
    let bytes = unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), p.len) };
    let mut d = zstd::bulk::Decompressor::new().expect("zstd decompressor");
    for i in 0..p.count {
        let out = &mut bytes[i * CHUNK..i * CHUNK + p.chunk_len(i)];
        let n = d
            .decompress_to_buffer(p.frame(i), out)
            .expect("ICU data decompresses");
        assert_eq!(n, p.chunk_len(i), "ICU data chunk {i} length");
    }
    release_packed();
    words
}

/// Decompress-on-first-touch backing for [`data`] (Linux and macOS).
///
/// [`map`] reserves the package's size as `PROT_NONE` memory and installs
/// a fault handler (`SIGSEGV`; on macOS also `SIGBUS`, which it raises for
/// protected pages). A read of a page that is not there yet faults; the
/// handler decompresses the chunks of that page and makes the page
/// readable in one atomic step, then returns, and the read is retried.
/// Another thread that reads the same page meanwhile faults too and waits
/// for the lock, or sees the whole page. Faults anywhere else go to the
/// handler that was installed before (Rust's stack-overflow check, else
/// the default action), so crashes stay crashes.
///
/// A fault makes one *unit* resident: the larger of a [`CHUNK`] and an OS
/// page (4 KiB on x86-64 Linux, so one chunk; 16 KiB on Apple silicon,
/// four chunks). The atomic step:
/// - Linux: decompress into a fresh scratch mapping and move it over the
///   reservation with `mremap(MREMAP_FIXED)`.
/// - macOS (no `mremap`): the reservation is a second mapping of the same
///   memory as a private writable view (`mach_vm_remap` without copy, once
///   at setup). The handler decompresses into the writable view, where no
///   reader looks, then `mprotect`s the reader's page to `PROT_READ`.
///   Pages ICU never reads are never touched in either view, so they cost
///   no memory.
///
/// The handler runs on the faulting thread's own stack (no `SA_ONSTACK`:
/// Rust's alternate signal stacks are a few KB, too small for zstd's
/// decoder). A stack overflow therefore ends the process with a plain
/// signal instead of Rust's "has overflowed its stack" message. Nothing
/// in the handler allocates or takes a lock other than its own spin lock:
/// the zstd context is created up front, and single-shot decompression
/// into a buffer does not allocate. It saves and restores `errno`. ICU
/// never hands its data to a system call, which would get `EFAULT`
/// instead of a fault. Debuggers stop on these faults; in gdb use
/// `handle SIGSEGV nostop noprint pass`; lldb stops on the Mach exception
/// before any signal, so set `LB_ICU_EAGER=1` there.
#[cfg(any(target_os = "linux", target_os = "macos"))]
mod lazy {
    use super::{CHUNK, Packed};
    use std::cell::UnsafeCell;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    /// More than enough for the 10.9 MB package (2,656 chunks); there are
    /// never more units than chunks.
    const MAX_CHUNKS: usize = 4096;

    /// The signals a read of a `PROT_NONE` page raises.
    #[cfg(target_os = "linux")]
    const SIGNALS: [libc::c_int; 1] = [libc::SIGSEGV];
    #[cfg(target_os = "macos")]
    const SIGNALS: [libc::c_int; 2] = [libc::SIGBUS, libc::SIGSEGV];

    static BASE: AtomicUsize = AtomicUsize::new(0);
    static SPAN: AtomicUsize = AtomicUsize::new(0);
    static LOCK: AtomicBool = AtomicBool::new(false);
    /// One flag per unit.
    static FILLED: [AtomicU8; MAX_CHUNKS] = [const { AtomicU8::new(0) }; MAX_CHUNKS];
    /// The handlers that were installed before, by index in [`SIGNALS`].
    static PREV: [OnceLock<libc::sigaction>; SIGNALS.len()] =
        [const { OnceLock::new() }; SIGNALS.len()];
    static STATE: OnceLock<State> = OnceLock::new();

    struct State {
        packed: Packed,
        /// Bytes per unit: a multiple of both [`CHUNK`] and the page size.
        unit: usize,
        /// macOS: start of the writable view of the reservation.
        #[cfg(target_os = "macos")]
        write_base: usize,
        /// Only used with [`LOCK`] held.
        dctx: UnsafeCell<zstd::bulk::Decompressor<'static>>,
    }

    // SAFETY: `dctx` is only touched by `fill`, with `LOCK` held, so never
    // from two threads at once; the other fields are read-only.
    unsafe impl Sync for State {}
    // SAFETY: as above; the decompressor owns plain heap memory.
    unsafe impl Send for State {}

    impl State {
        fn chunks_per_unit(&self) -> usize {
            self.unit / CHUNK
        }

        /// Chunks of unit `u` that hold data (the last unit may hold fewer).
        fn chunks_in(&self, u: usize) -> std::ops::Range<usize> {
            let first = u * self.chunks_per_unit();
            first..(first + self.chunks_per_unit()).min(self.packed.count)
        }

        /// Decompress the chunks of unit `u` into `out` (the unit's bytes).
        fn decompress_unit(&self, u: usize, out: &mut [u8]) -> bool {
            // SAFETY: only called by `place`, with LOCK held, so this is
            // the only user of `dctx`.
            let dctx = unsafe { &mut *self.dctx.get() };
            let first = u * self.chunks_per_unit();
            self.chunks_in(u).all(|i| {
                let at = (i - first) * CHUNK;
                let len = self.packed.chunk_len(i);
                matches!(dctx.decompress_to_buffer(self.packed.frame(i), &mut out[at..at + len]), Ok(n) if n == len)
            })
        }
    }

    /// `VM_FLAGS_ANYWHERE` from <mach/vm_statistics.h> (the libc crate's
    /// Mach items are deprecated in favour of another crate).
    #[cfg(target_os = "macos")]
    const VM_FLAGS_ANYWHERE: libc::c_int = 1;

    #[cfg(target_os = "macos")]
    unsafe extern "C" {
        /// The task port that `mach_task_self()` (a macro) reads.
        static mach_task_self_: libc::mach_port_t;
        // <mach/mach_vm.h>; not in the libc crate.
        fn mach_vm_remap(
            target_task: libc::vm_map_t,
            target_address: *mut libc::mach_vm_address_t,
            size: libc::mach_vm_size_t,
            mask: libc::mach_vm_offset_t,
            flags: libc::c_int,
            src_task: libc::vm_map_t,
            src_address: libc::mach_vm_address_t,
            copy: libc::boolean_t,
            cur_protection: *mut libc::vm_prot_t,
            max_protection: *mut libc::vm_prot_t,
            inheritance: libc::vm_inherit_t,
        ) -> libc::kern_return_t;
    }

    /// Reserve `span` bytes that read as `PROT_NONE`. Returns the
    /// reservation and, on macOS, its writable view.
    #[cfg(target_os = "linux")]
    fn reserve(span: usize) -> Option<(usize, usize)> {
        // SAFETY: a fresh private anonymous reservation; nothing else
        // refers to it. It is never unmapped (ICU keeps pointers into it).
        let base = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                span,
                libc::PROT_NONE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_NORESERVE,
                -1,
                0,
            )
        };
        (base != libc::MAP_FAILED).then_some((base as usize, 0))
    }

    #[cfg(target_os = "macos")]
    fn reserve(span: usize) -> Option<(usize, usize)> {
        // SAFETY: a fresh private anonymous mapping (the writable view),
        // then a second mapping of the same memory (`copy` false) made
        // `PROT_NONE` (the reservation). Nothing else refers to either;
        // both live for the process once this returns them.
        unsafe {
            let write = libc::mmap(
                std::ptr::null_mut(),
                span,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANON,
                -1,
                0,
            );
            if write == libc::MAP_FAILED {
                return None;
            }
            let mut base: libc::mach_vm_address_t = 0;
            let (mut cur, mut max): (libc::vm_prot_t, libc::vm_prot_t) = (0, 0);
            let kr = mach_vm_remap(
                mach_task_self_,
                &mut base,
                span as libc::mach_vm_size_t,
                0,
                VM_FLAGS_ANYWHERE,
                mach_task_self_,
                write as libc::mach_vm_address_t,
                0,
                &mut cur,
                &mut max,
                libc::VM_INHERIT_NONE as libc::vm_inherit_t,
            );
            if kr != libc::KERN_SUCCESS {
                libc::munmap(write, span);
                return None;
            }
            if libc::mprotect(base as *mut libc::c_void, span, libc::PROT_NONE) != 0 {
                libc::munmap(base as *mut libc::c_void, span);
                libc::munmap(write, span);
                return None;
            }
            Some((base as usize, write as usize))
        }
    }

    /// Undo [`reserve`] (setup failed after it).
    fn unreserve(base: usize, _write: usize, span: usize) {
        // SAFETY: the mappings made by `reserve`, not yet published.
        unsafe {
            libc::munmap(base as *mut libc::c_void, span);
            #[cfg(target_os = "macos")]
            libc::munmap(_write as *mut libc::c_void, span);
        }
    }

    /// Reserve the package and install the fault handler. `None` if either
    /// fails (the caller decompresses eagerly instead).
    pub(super) fn map(packed: Packed) -> Option<&'static [u8]> {
        // SAFETY: sysconf has no preconditions.
        let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) };
        if packed.count > MAX_CHUNKS || page <= 0 {
            return None;
        }
        let page = page as usize;
        let unit = page.max(CHUNK);
        if !unit.is_multiple_of(page) || !unit.is_multiple_of(CHUNK) {
            return None;
        }
        let dctx = zstd::bulk::Decompressor::new().ok()?;
        let span = (packed.count * CHUNK).next_multiple_of(unit);
        let (base, write) = reserve(span)?;
        let state = State {
            packed,
            unit,
            #[cfg(target_os = "macos")]
            write_base: write,
            dctx: UnsafeCell::new(dctx),
        };
        if STATE.set(state).is_err() {
            unreserve(base, write, span);
            return None;
        }
        // SAFETY: plain sigaction calls with zeroed, then filled structs.
        // The handler only acts on addresses inside the reservation, which
        // is published (BASE/SPAN) before the handler is installed.
        unsafe {
            for (k, &sig) in SIGNALS.iter().enumerate() {
                let mut old: libc::sigaction = std::mem::zeroed();
                if libc::sigaction(sig, std::ptr::null(), &mut old) != 0 {
                    unreserve(base, write, span);
                    return None;
                }
                let _ = PREV[k].set(old);
            }
            SPAN.store(span, Ordering::Release);
            BASE.store(base, Ordering::Release);
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_fault as *const () as libc::sighandler_t;
            sa.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut sa.sa_mask);
            for (k, &sig) in SIGNALS.iter().enumerate() {
                if libc::sigaction(sig, &sa, std::ptr::null_mut()) != 0 {
                    // Put back the handlers already replaced; then nothing
                    // can reach the reservation any more.
                    for (j, &s) in SIGNALS.iter().enumerate().take(k) {
                        if let Some(p) = PREV[j].get() {
                            libc::sigaction(s, p, std::ptr::null_mut());
                        }
                    }
                    BASE.store(0, Ordering::Release);
                    unreserve(base, write, span);
                    return None;
                }
            }
            Some(std::slice::from_raw_parts(base as *const u8, packed.len))
        }
    }

    /// This thread's `errno`.
    fn errno() -> *mut libc::c_int {
        // SAFETY: both return this thread's errno slot; no preconditions.
        #[cfg(target_os = "linux")]
        unsafe {
            libc::__errno_location()
        }
        #[cfg(target_os = "macos")]
        unsafe {
            libc::__error()
        }
    }

    extern "C" fn on_fault(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        // SAFETY: the kernel passes a valid siginfo for SIGSEGV and SIGBUS.
        let addr = unsafe { (*info).si_addr() } as usize;
        let base = BASE.load(Ordering::Acquire);
        let span = SPAN.load(Ordering::Acquire);
        if base != 0
            && addr >= base
            && addr < base + span
            && let Some(state) = STATE.get()
        {
            // SAFETY: errno is thread-local; restored before returning.
            let saved = unsafe { *errno() };
            let ok = fill(state, (addr - base) / state.unit, base);
            // SAFETY: as above.
            unsafe { *errno() = saved };
            if ok {
                return;
            }
            let msg = b"lightbrowser: could not page in ICU data\n";
            // SAFETY: write(2) and abort(3) are async-signal-safe.
            unsafe {
                libc::write(2, msg.as_ptr().cast(), msg.len());
                libc::abort();
            }
        }
        forward(sig, info, ctx);
    }

    /// Hand a fault outside the package to the previous handler.
    fn forward(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        let prev = SIGNALS
            .iter()
            .position(|&s| s == sig)
            .and_then(|k| PREV[k].get().copied());
        match prev {
            Some(p)
                if p.sa_sigaction != libc::SIG_DFL
                    && p.sa_sigaction != libc::SIG_IGN
                    && p.sa_flags & libc::SA_SIGINFO != 0 =>
            {
                // SAFETY: a SA_SIGINFO handler has this signature.
                let f: extern "C" fn(libc::c_int, *mut libc::siginfo_t, *mut libc::c_void) =
                    unsafe { std::mem::transmute(p.sa_sigaction) };
                f(sig, info, ctx);
            }
            Some(p) if p.sa_sigaction != libc::SIG_DFL && p.sa_sigaction != libc::SIG_IGN => {
                // SAFETY: a plain handler has this signature.
                let f: extern "C" fn(libc::c_int) = unsafe { std::mem::transmute(p.sa_sigaction) };
                f(sig);
            }
            _ => {
                // Default action: reinstall it and return; the faulting
                // instruction runs again and the process ends as usual.
                // SAFETY: sigaction is async-signal-safe.
                unsafe {
                    let mut dfl: libc::sigaction = std::mem::zeroed();
                    dfl.sa_sigaction = libc::SIG_DFL;
                    libc::sigaction(sig, &dfl, std::ptr::null_mut());
                }
            }
        }
    }

    /// Make unit `u` resident. Returns false if it cannot be.
    fn fill(state: &State, u: usize, base: usize) -> bool {
        if u * state.chunks_per_unit() >= state.packed.count {
            return false;
        }
        while LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        // SAFETY: LOCK is held, and `u` is a unit of the reservation at
        // `base` made by `map`.
        let ok = FILLED[u].load(Ordering::Acquire) == 1 || unsafe { place(state, u, base) };
        if ok {
            FILLED[u].store(1, Ordering::Release);
        }
        LOCK.store(false, Ordering::Release);
        ok
    }

    /// Decompress unit `u` into a scratch mapping and move it over the
    /// reservation at `base + u * unit`.
    ///
    /// # Safety
    /// `base` is the reservation made by `map`, `u` a unit of it, and the
    /// caller holds `LOCK`.
    #[cfg(target_os = "linux")]
    unsafe fn place(state: &State, u: usize, base: usize) -> bool {
        let unit = state.unit;
        // SAFETY: fresh private anonymous mapping, only used here.
        let scratch = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                unit,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if scratch == libc::MAP_FAILED {
            return false;
        }
        // SAFETY: `scratch` is `unit` writable bytes owned here.
        let out = unsafe { std::slice::from_raw_parts_mut(scratch.cast::<u8>(), unit) };
        let ok = state.decompress_unit(u, out);
        // SAFETY: `scratch` is ours; on success it replaces the reserved
        // pages of unit u (MREMAP_FIXED unmaps them in the same step);
        // ICU only ever reads the package, so it is made read-only first.
        unsafe {
            if !ok || libc::mprotect(scratch, unit, libc::PROT_READ) != 0 {
                libc::munmap(scratch, unit);
                return false;
            }
            super::release_packed();
            let target = (base + u * unit) as *mut libc::c_void;
            let moved = libc::mremap(
                scratch,
                unit,
                unit,
                libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
                target,
            );
            if moved != target {
                libc::munmap(scratch, unit);
                return false;
            }
        }
        true
    }

    /// Decompress unit `u` through the writable view, then make the
    /// reservation's page readable.
    ///
    /// # Safety
    /// As for the Linux `place`.
    #[cfg(target_os = "macos")]
    unsafe fn place(state: &State, u: usize, base: usize) -> bool {
        let unit = state.unit;
        // SAFETY: the writable view of unit u: `unit` bytes that no reader
        // can see until the mprotect below, and only written with LOCK held.
        let out = unsafe {
            std::slice::from_raw_parts_mut((state.write_base + u * unit) as *mut u8, unit)
        };
        // SAFETY: a page-aligned range inside the reservation; mprotect is
        // async-signal-safe.
        state.decompress_unit(u, out)
            && unsafe {
                libc::mprotect(
                    (base + u * unit) as *mut libc::c_void,
                    unit,
                    libc::PROT_READ,
                )
            } == 0
    }

    /// Whether [`map`] succeeded.
    pub(super) fn active() -> bool {
        BASE.load(Ordering::Acquire) != 0
    }

    /// Chunks made resident so far.
    pub(super) fn resident() -> usize {
        let Some(state) = STATE.get() else {
            return 0;
        };
        FILLED
            .iter()
            .enumerate()
            .filter(|(_, f)| f.load(Ordering::Acquire) == 1)
            .map(|(u, _)| state.chunks_in(u).len())
            .sum()
    }
}

/// How many [`CHUNK`]s of [`data`] are resident: `Some` when the lazy
/// path is in use (Linux, macOS), `None` when the package was
/// decompressed at once.
pub fn resident_chunks() -> Option<usize> {
    let _ = data();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    if lazy::active() {
        return Some(lazy::resident());
    }
    None
}

/// Registers the ICU data with ICU. Called once by `init_v8`, before
/// `V8::initialize` and before any ICU use.
pub(crate) fn install() {
    if let Err(code) = v8::icu::set_common_data_78(data()) {
        // A failure leaves Intl without data (V8 aborts on first use), so
        // stop here with the reason instead.
        panic!("udata_setCommonData failed: ICU error {code}");
    }
}

/// Extracts the ICU data from the prebuilt `librusty_v8.a`: the member
/// [`MEMBER`] (an ELF64 object on Linux, a 64-bit Mach-O object on macOS),
/// from symbol [`SYMBOL`] to the end of its section. Used by the
/// `icu_pack` example and the `icu_data` tests.
pub fn extract_from_archive(ar: &[u8]) -> Result<Vec<u8>, String> {
    let obj = archive_members(ar)?
        .into_iter()
        .find(|(n, _)| *n == MEMBER.as_bytes())
        .map(|(_, body)| body)
        .ok_or_else(|| format!("{MEMBER} not found in archive"))?;
    if obj.starts_with(&MACHO64_MAGIC) {
        macho_symbol_section_tail(obj, SYMBOL)
    } else {
        elf_symbol_section_tail(obj, SYMBOL)
    }
}

/// `len` bytes of `b` from `start`, or `None` when that runs past the end
/// (or the sum overflows): offsets and lengths read from a file may be
/// anything.
fn slice_at(b: &[u8], start: usize, len: usize) -> Option<&[u8]> {
    b.get(start..)?.get(..len)
}

/// One `ar` member: its name and its body.
pub type ArMember<'a> = (&'a [u8], &'a [u8]);

/// The members of an `ar` archive, in order, including the symbol index.
/// Reads both the GNU format (Linux: `/` index, `//` long-name table,
/// `name/` and `/<offset>` names) and the BSD format (macOS: `__.SYMDEF`
/// index, `#1/<len>` names stored at the start of the body).
pub fn archive_members(ar: &[u8]) -> Result<Vec<ArMember<'_>>, String> {
    if !ar.starts_with(b"!<arch>\n") {
        return Err("not an ar archive".into());
    }
    let mut out = Vec::new();
    let mut pos = 8;
    let mut long_names: &[u8] = &[];
    while pos + 60 <= ar.len() {
        let hdr = &ar[pos..pos + 60];
        let size: usize = std::str::from_utf8(&hdr[48..58])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or("bad ar member size")?;
        let mut body = slice_at(ar, pos + 60, size).ok_or("truncated ar member")?;
        pos += 60 + size + (size & 1);
        let raw = std::str::from_utf8(&hdr[0..16])
            .map_err(|_| "bad ar name")?
            .trim_end();
        let name: &[u8] = if raw == "//" {
            long_names = body;
            continue;
        } else if let Some(len) = raw.strip_prefix("#1/") {
            // BSD: the name is the first `len` bytes of the body, padded
            // with NULs.
            let len: usize = len.parse().map_err(|_| "bad BSD name length")?;
            let n = body.get(..len).ok_or("bad BSD name length")?;
            body = &body[len..];
            &n[..n.iter().position(|&c| c == 0).unwrap_or(n.len())]
        } else if let Some(off) = raw.strip_prefix('/').and_then(|o| o.parse::<usize>().ok()) {
            let rest = long_names.get(off..).ok_or("bad long name offset")?;
            let n = &rest[..rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len())];
            n.strip_suffix(b"/").unwrap_or(n)
        } else if raw == "/" || raw == "/SYM64/" {
            raw.as_bytes()
        } else {
            raw.trim_end_matches('/').as_bytes()
        };
        out.push((name, body));
    }
    Ok(out)
}

/// The global symbol names of an `ar` archive's index: GNU `/` or
/// `/SYM64/`, or BSD `__.SYMDEF` / `__.SYMDEF SORTED` (and their `_64`
/// forms). Names are given as the C or C++ source spells them: the
/// leading `_` that Mach-O adds to every symbol is removed for a BSD
/// index. Used by the `unused_v8` tests.
pub fn archive_symbols(ar: &[u8]) -> Result<Vec<Vec<u8>>, String> {
    let members = archive_members(ar)?;
    let &(name, body) = members.first().ok_or("empty archive")?;
    let bad = || "malformed symbol index".to_string();
    let int = |o: usize, w: usize, big: bool| -> Result<usize, String> {
        let b = slice_at(body, o, w).ok_or_else(bad)?;
        let fold = |a: u64, &x: &u8| (a << 8) | u64::from(x);
        let v = if big {
            b.iter().fold(0, fold)
        } else {
            b.iter().rev().fold(0, fold)
        };
        usize::try_from(v).map_err(|_| bad())
    };
    let cstr =
        |s: &[u8]| -> Vec<u8> { s[..s.iter().position(|&c| c == 0).unwrap_or(s.len())].to_vec() };
    match name {
        b"/" | b"/SYM64/" => {
            // Big-endian count, one member offset per symbol, then the
            // NUL-terminated names.
            let w = if name == b"/" { 4 } else { 8 };
            let count = int(0, w, true)?;
            let skip = count.checked_mul(w).and_then(|n| n.checked_add(w));
            let names = body.get(skip.ok_or_else(bad)?..).ok_or_else(bad)?;
            Ok(names
                .split(|&b| b == 0)
                .take(count)
                .map(<[u8]>::to_vec)
                .collect())
        }
        b"__.SYMDEF" | b"__.SYMDEF SORTED" | b"__.SYMDEF_64" | b"__.SYMDEF_64 SORTED" => {
            // Host-endian (little-endian on every Mac we build for): byte
            // length of the `ranlib` entries, the entries (string offset,
            // member offset), byte length of the strings, the strings.
            let w = if name.starts_with(b"__.SYMDEF_64") {
                8
            } else {
                4
            };
            let ranlib = int(0, w, false)?;
            // `ranlib` bytes of entries must fit in the body before the
            // string length is read, so the sums below cannot overflow.
            slice_at(body, w, ranlib).ok_or_else(bad)?;
            let strsize = int(w + ranlib, w, false)?;
            let strings = slice_at(body, w + ranlib + w, strsize).ok_or_else(bad)?;
            (w..w + ranlib)
                .step_by(2 * w)
                .map(|e| {
                    let s = cstr(strings.get(int(e, w, false)?..).ok_or_else(bad)?);
                    Ok(s.strip_prefix(b"_").map(<[u8]>::to_vec).unwrap_or(s))
                })
                .collect()
        }
        _ => Err("archive has no symbol index".into()),
    }
}

/// `MH_MAGIC_64` as a little-endian Mach-O object stores it.
const MACHO64_MAGIC: [u8; 4] = [0xcf, 0xfa, 0xed, 0xfe];

/// [`elf_symbol_section_tail`] for a little-endian 64-bit Mach-O object,
/// whose symbol names carry a leading `_`.
fn macho_symbol_section_tail(obj: &[u8], symbol: &str) -> Result<Vec<u8>, String> {
    const LC_SYMTAB: usize = 0x2;
    const LC_SEGMENT_64: usize = 0x19;
    // Section types with no file data: S_ZEROFILL, S_GB_ZEROFILL,
    // S_THREAD_LOCAL_ZEROFILL. Their `offset` is 0.
    const ZEROFILL: [usize; 3] = [0x1, 0xc, 0x12];
    let bad = || "malformed Mach-O".to_string();
    let u32_at = |o: usize| {
        slice_at(obj, o, 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            .ok_or_else(bad)
    };
    let u64_at = |o: usize| {
        slice_at(obj, o, 8)
            .and_then(|b| usize::try_from(u64::from_le_bytes(b.try_into().unwrap())).ok())
            .ok_or_else(bad)
    };
    // (addr, size, file offset, flags) per section, in ordinal order.
    let mut sections = Vec::new();
    let mut symtab = None;
    let mut lc = 32; // after mach_header_64
    for _ in 0..u32_at(16)? {
        let (cmd, cmdsize) = (u32_at(lc)?, u32_at(lc + 4)?);
        if cmd == LC_SEGMENT_64 {
            for k in 0..u32_at(lc + 64)? {
                let s = lc + 72 + 80 * k; // section_64 entries
                sections.push((
                    u64_at(s + 32)?,
                    u64_at(s + 40)?,
                    u32_at(s + 48)?,
                    u32_at(s + 64)?,
                ));
            }
        } else if cmd == LC_SYMTAB {
            symtab = Some((u32_at(lc + 8)?, u32_at(lc + 12)?, u32_at(lc + 16)?));
        }
        if cmdsize < 8 {
            return Err(bad());
        }
        lc += cmdsize;
    }
    let (symoff, nsyms, stroff) = symtab.ok_or("no LC_SYMTAB")?;
    let want = format!("_{symbol}");
    for k in 0..nsyms {
        let e = symoff + 16 * k; // nlist_64
        let strx = stroff.checked_add(u32_at(e)?).ok_or_else(bad)?;
        let name = obj.get(strx..).ok_or_else(bad)?;
        let name = &name[..name.iter().position(|&c| c == 0).ok_or_else(bad)?];
        if name != want.as_bytes() {
            continue;
        }
        let n_type = *obj.get(e + 4).ok_or_else(bad)?;
        let n_sect = usize::from(*obj.get(e + 5).ok_or_else(bad)?);
        let value = u64_at(e + 8)?;
        if n_type & 0x0e != 0x0e || n_sect == 0 {
            return Err(format!("{symbol} is not defined in a section"));
        }
        let (addr, size, off, flags) = *sections.get(n_sect - 1).ok_or_else(bad)?;
        if ZEROFILL.contains(&(flags & 0xff)) || (off == 0 && size > 0) || value < addr {
            return Err(format!("{symbol} is not in a section with file data"));
        }
        let (start, end) = (off.checked_add(value - addr), off.checked_add(size));
        return start
            .zip(end)
            .and_then(|(start, end)| obj.get(start..end))
            .map(<[u8]>::to_vec)
            .ok_or_else(bad);
    }
    Err(format!("symbol {symbol} not found"))
}

fn elf_symbol_section_tail(obj: &[u8], symbol: &str) -> Result<Vec<u8>, String> {
    let u16_at = |o: usize| obj.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    let u32_at = |o: usize| {
        obj.get(o..o + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
    };
    let u64_at = |o: usize| {
        obj.get(o..o + 8)
            .map(|b| u64::from_le_bytes(b.try_into().unwrap()))
    };
    if !obj.starts_with(b"\x7fELF\x02\x01") {
        return Err("not a little-endian ELF64 object".into());
    }
    let bad = || "malformed ELF".to_string();
    let shoff = u64_at(0x28).ok_or_else(bad)? as usize;
    let shentsize = u16_at(0x3a).ok_or_else(bad)? as usize;
    let shnum = u16_at(0x3c).ok_or_else(bad)? as usize;
    // (type, offset, size, link, entsize) per section.
    let section = |i: usize| -> Option<(u32, usize, usize, usize, usize)> {
        let s = shoff + i * shentsize;
        Some((
            u32_at(s + 4)?,
            u64_at(s + 0x18)? as usize,
            u64_at(s + 0x20)? as usize,
            u32_at(s + 0x28)? as usize,
            u64_at(s + 0x38)? as usize,
        ))
    };
    for i in 0..shnum {
        let (ty, off, size, link, entsize) = section(i).ok_or_else(bad)?;
        if ty != 2 || entsize != 24 {
            continue; // not SHT_SYMTAB
        }
        let (_, stroff, _, _, _) = section(link).ok_or_else(bad)?;
        for k in 0..size / entsize {
            let e = off + k * entsize;
            let name_off = u32_at(e).ok_or_else(bad)? as usize;
            let name = obj.get(stroff + name_off..).ok_or_else(bad)?;
            let name = &name[..name.iter().position(|&c| c == 0).ok_or_else(bad)?];
            if name != symbol.as_bytes() {
                continue;
            }
            let shndx = u16_at(e + 6).ok_or_else(bad)? as usize;
            let value = u64_at(e + 8).ok_or_else(bad)? as usize;
            let (sty, soff, ssize, _, _) = section(shndx).ok_or_else(bad)?;
            if sty != 1 {
                return Err(format!("{symbol} is not in a PROGBITS section"));
            }
            return obj
                .get(soff + value..soff + ssize)
                .map(<[u8]>::to_vec)
                .ok_or_else(bad);
        }
    }
    Err(format!("symbol {symbol} not found"))
}

/// Where the `v8` crate's build script put the prebuilt archive for the
/// build that produced `exe` (`target/<profile>/gn_out/obj/librusty_v8.a`),
/// or `RUSTY_V8_ARCHIVE` if that names a file.
pub fn find_archive(exe: &std::path::Path) -> Option<std::path::PathBuf> {
    if let Some(p) = std::env::var_os("RUSTY_V8_ARCHIVE").map(std::path::PathBuf::from)
        && p.is_file()
        && p.extension().is_some_and(|e| e == "a")
    {
        return Some(p);
    }
    exe.ancestors()
        .map(|d| d.join("gn_out/obj/librusty_v8.a"))
        .find(|p| p.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn member(name: &str, body: &[u8]) -> Vec<u8> {
        let mut m = format!(
            "{name:<16}{:<12}{:<6}{:<6}{:<8}{:<10}`\n",
            0,
            0,
            0,
            644,
            body.len()
        )
        .into_bytes();
        m.extend_from_slice(body);
        if body.len() % 2 == 1 {
            m.push(b'\n');
        }
        m
    }

    #[test]
    fn reads_gnu_archives() {
        let mut index = 2u32.to_be_bytes().to_vec();
        index.extend_from_slice(&[0; 8]);
        index.extend_from_slice(b"icudt78_dat\0_ZN2v84Stub\0");
        let mut ar = b"!<arch>\n".to_vec();
        ar.extend(member("/", &index));
        ar.extend(member("//", b"a_rather_long_member_name.o/\n"));
        ar.extend(member("short.o/", b"abc"));
        ar.extend(member("/0", b"long body"));
        let m = archive_members(&ar).unwrap();
        let names: Vec<_> = m.iter().map(|m| m.0).collect();
        assert_eq!(
            names,
            [&b"/"[..], b"short.o", b"a_rather_long_member_name.o"]
        );
        assert_eq!((m[1].1, m[2].1), (&b"abc"[..], &b"long body"[..]));
        assert_eq!(
            archive_symbols(&ar).unwrap(),
            [b"icudt78_dat".to_vec(), b"_ZN2v84Stub".to_vec()]
        );
    }

    #[test]
    fn reads_bsd_archives() {
        let strings = b"_icudt78_dat\0__ZN2v84Stub\0\0\0";
        let mut index = 16u32.to_le_bytes().to_vec();
        for (strx, off) in [(0u32, 8u32), (13, 8)] {
            index.extend_from_slice(&strx.to_le_bytes());
            index.extend_from_slice(&off.to_le_bytes());
        }
        index.extend_from_slice(&(strings.len() as u32).to_le_bytes());
        index.extend_from_slice(strings);
        let mut body = b"__.SYMDEF SORTED\0\0\0\0".to_vec();
        body.extend_from_slice(&index);
        let mut ar = b"!<arch>\n".to_vec();
        ar.extend(member("#1/20", &body));
        let mut body = b"icudtl_dat.o\0\0\0\0".to_vec();
        body.extend_from_slice(b"object");
        ar.extend(member("#1/16", &body));
        let m = archive_members(&ar).unwrap();
        assert_eq!(m[0].0, b"__.SYMDEF SORTED");
        assert_eq!((m[1].0, m[1].1), (&b"icudtl_dat.o"[..], &b"object"[..]));
        assert_eq!(
            archive_symbols(&ar).unwrap(),
            [b"icudt78_dat".to_vec(), b"_ZN2v84Stub".to_vec()]
        );
    }

    #[test]
    fn reads_gnu_sym64_index() {
        let mut index = 2u64.to_be_bytes().to_vec();
        index.extend_from_slice(&[0; 16]);
        index.extend_from_slice(b"icudt78_dat\0_ZN2v84Stub\0");
        let mut ar = b"!<arch>\n".to_vec();
        ar.extend(member("/SYM64/", &index));
        ar.extend(member("short.o/", b"abc"));
        assert_eq!(archive_members(&ar).unwrap()[0].0, b"/SYM64/");
        assert_eq!(
            archive_symbols(&ar).unwrap(),
            [b"icudt78_dat".to_vec(), b"_ZN2v84Stub".to_vec()]
        );
        // A count whose offsets would run past `usize` is an error.
        let mut ar = b"!<arch>\n".to_vec();
        ar.extend(member("/SYM64/", &u64::MAX.to_be_bytes()));
        assert!(archive_symbols(&ar).is_err());
    }

    /// A BSD `__.SYMDEF_64` index (8-byte `ranlib` fields) for `strings`,
    /// with `entries` (string offset, member offset), stored as `#1/16`.
    fn symdef64(ranlib_len: u64, entries: &[(u64, u64)], strings: &[u8]) -> Vec<u8> {
        let mut body = b"__.SYMDEF_64\0\0\0\0".to_vec();
        body.extend_from_slice(&ranlib_len.to_le_bytes());
        for (strx, off) in entries {
            body.extend_from_slice(&strx.to_le_bytes());
            body.extend_from_slice(&off.to_le_bytes());
        }
        body.extend_from_slice(&(strings.len() as u64).to_le_bytes());
        body.extend_from_slice(strings);
        member("#1/16", &body)
    }

    #[test]
    fn reads_bsd_symdef64_short_names_and_odd_sizes() {
        let mut ar = b"!<arch>\n".to_vec();
        ar.extend(symdef64(
            32,
            &[(0, 8), (13, 8)],
            b"_icudt78_dat\0__ZN2v84Stub\0\0\0",
        ));
        // A short name in the header, an odd body, then a `#1/` member
        // whose name and body add up to an odd size.
        ar.extend(member("short.o", b"abc"));
        ar.extend(member("#1/12", b"icudtl_dat.oodd"));
        ar.extend(member("last.o", b"end"));
        let m = archive_members(&ar).unwrap();
        let names: Vec<_> = m.iter().map(|m| m.0).collect();
        assert_eq!(
            names,
            [&b"__.SYMDEF_64"[..], b"short.o", b"icudtl_dat.o", b"last.o"]
        );
        assert_eq!(
            (m[1].1, m[2].1, m[3].1),
            (&b"abc"[..], &b"odd"[..], &b"end"[..])
        );
        assert_eq!(
            archive_symbols(&ar).unwrap(),
            [b"icudt78_dat".to_vec(), b"_ZN2v84Stub".to_vec()]
        );
        // Lengths that would overflow are errors, not panics.
        for ranlib_len in [u64::MAX, u64::MAX - 7] {
            let mut ar = b"!<arch>\n".to_vec();
            ar.extend(symdef64(ranlib_len, &[], b""));
            assert!(archive_symbols(&ar).is_err());
        }
    }

    /// A minimal 64-bit Mach-O object: one `LC_SEGMENT_64` with `__text`
    /// ("code" at address 0) and `__const` (`sect2_size` bytes
    /// "headDATAtail" at address 0x10, file offset `sect2_off`, flags
    /// `sect2_flags`), and an `LC_SYMTAB` with `_other` in section 1 and
    /// `_icudt78_dat` in section 2 at `value`.
    fn macho(sect2_flags: u32, sect2_off: Option<u32>, sect2_size: u64, value: u64) -> Vec<u8> {
        const DATA: usize = 32 + 232 + 24;
        let (symoff, stroff) = (DATA + 16, DATA + 16 + 32);
        let mut o = Vec::new();
        let u32s = |o: &mut Vec<u8>, v: &[u32]| v.iter().for_each(|x| o.extend(x.to_le_bytes()));
        let u64s = |o: &mut Vec<u8>, v: &[u64]| v.iter().for_each(|x| o.extend(x.to_le_bytes()));
        let name16 = |o: &mut Vec<u8>, n: &str| {
            o.extend(n.as_bytes());
            o.extend(std::iter::repeat_n(0, 16 - n.len()));
        };
        o.extend(MACHO64_MAGIC);
        u32s(&mut o, &[0x0100_000c, 0, 1, 2, 256, 0, 0]);
        // LC_SEGMENT_64 with two section_64 entries.
        u32s(&mut o, &[0x19, 232]);
        name16(&mut o, "");
        u64s(&mut o, &[0, 0x1c, DATA as u64, 16]);
        u32s(&mut o, &[7, 7, 2, 0]);
        let sections = [
            ("__text", 0, 4, Some(DATA as u32), 0x8000_0400),
            ("__const", 0x10, sect2_size, sect2_off, sect2_flags),
        ];
        for (name, addr, size, off, flags) in sections {
            name16(&mut o, name);
            name16(&mut o, "__TEXT");
            u64s(&mut o, &[addr, size]);
            u32s(
                &mut o,
                &[off.unwrap_or(DATA as u32 + 4), 0, 0, 0, flags, 0, 0, 0],
            );
        }
        // LC_SYMTAB.
        u32s(&mut o, &[0x2, 24, symoff as u32, 2, stroff as u32, 21]);
        assert_eq!(o.len(), DATA);
        o.extend(b"codeheadDATAtail");
        for (strx, sect, v) in [(1u32, 1u8, 0u64), (8, 2, value)] {
            u32s(&mut o, &[strx]);
            o.extend([0x0f, sect, 0, 0]);
            u64s(&mut o, &[v]);
        }
        o.extend(b"\0_other\0_icudt78_dat\0");
        o
    }

    #[test]
    fn reads_macho_objects() {
        let obj = macho(0, None, 12, 0x14);
        assert_eq!(
            macho_symbol_section_tail(&obj, SYMBOL).unwrap(),
            b"DATAtail"
        );
        assert_eq!(macho_symbol_section_tail(&obj, "other").unwrap(), b"code");
        assert!(macho_symbol_section_tail(&obj, "missing").is_err());
        // Through a BSD archive, as on macOS.
        let mut body = b"icudtl_dat.o\0\0\0\0".to_vec();
        body.extend(&obj);
        let mut ar = b"!<arch>\n".to_vec();
        ar.extend(symdef64(16, &[(0, 8)], b"_icudt78_dat\0\0\0\0"));
        ar.extend(member("#1/16", &body));
        assert_eq!(extract_from_archive(&ar).unwrap(), b"DATAtail");
    }

    #[test]
    fn rejects_macho_sections_without_data_and_bad_offsets() {
        // S_ZEROFILL, S_GB_ZEROFILL, S_THREAD_LOCAL_ZEROFILL, and a section
        // with data but file offset 0 (the header).
        for (flags, off) in [
            (0x1, Some(0)),
            (0xc, Some(0)),
            (0x12, Some(0)),
            (0, Some(0)),
        ] {
            let err = macho_symbol_section_tail(&macho(flags, off, 12, 0x14), SYMBOL);
            assert!(err.unwrap_err().contains("file data"), "flags {flags:#x}");
        }
        // Values that would overflow the offset sums.
        for (size, value) in [(12, u64::MAX), (u64::MAX, 0x14), (12, 0x10 + 13)] {
            let err = macho_symbol_section_tail(&macho(0, None, size, value), SYMBOL);
            assert_eq!(err.unwrap_err(), "malformed Mach-O");
        }
    }
}
