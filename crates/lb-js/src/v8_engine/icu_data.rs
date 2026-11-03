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
//! On Linux the 10.9 MB package is reserved as inaccessible memory and
//! each chunk is decompressed the first time ICU touches it (see `lazy`),
//! like a file mapping that pages in on demand: a page load reads about
//! 120 KB of it (the table of contents and a few items such as the root
//! locale and time zone data), so about 5 chunks (0.6 MB, < 1 ms) become
//! resident instead of all of it (10.9 MB, ~15 ms). If the reservation or
//! the fault handler cannot be set up, or on other systems, the whole
//! package is decompressed into the heap at once.

use std::sync::OnceLock;

/// Name of the data symbol in the prebuilt archive (ICU major version 78,
/// matching `v8::icu::set_common_data_78`).
pub const SYMBOL: &str = "icudt78_dat";

/// Archive member that holds the data.
pub const MEMBER: &str = "icudtl_dat.o";

/// The archive's ICU data in chunks (see [`pack`]).
pub const PACKED: &[u8] = include_bytes!("../../icu/icudt78.dat.zch");

/// Bytes of ICU data per compressed frame (a multiple of the page size).
/// Smaller chunks keep less of the package resident but compress worse:
/// 128 KiB costs 0.43 MB of binary over one frame for the whole package
/// (64 KiB: 0.58 MB, 256 KiB: 0.35 MB).
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
/// aligned. On Linux its chunks are decompressed when first read (see the
/// module docs); elsewhere, or if that cannot be set up, all at once here.
pub fn data() -> &'static [u8] {
    DATA.get_or_init(|| {
        #[cfg(target_os = "linux")]
        if let Some(d) = lazy::map(Packed::get()) {
            return d;
        }
        let words: &'static [u128] = Box::leak(decompress_all());
        // SAFETY: the leaked allocation holds at least `len` initialised
        // bytes, lives for the process, and u8 has no alignment or validity
        // requirements.
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), Packed::get().len) }
    })
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
    words
}

/// Decompress-on-first-touch backing for [`data`] (Linux).
///
/// [`map`] reserves the package's size as `PROT_NONE` memory and installs
/// a `SIGSEGV` handler. A read of a chunk that is not there yet faults; the
/// handler decompresses that chunk into a fresh scratch mapping and moves
/// it over the reserved range with `mremap` (one atomic step: another
/// thread either faults too and waits for the lock, or sees the whole
/// chunk), then returns, and the read is retried. Faults anywhere else go
/// to the handler that was installed before (Rust's stack-overflow check,
/// else the default action), so crashes stay crashes.
///
/// The handler runs on the faulting thread's own stack (no `SA_ONSTACK`:
/// Rust's alternate signal stacks are a few KB, too small for zstd's
/// decoder). A stack overflow therefore ends the process with a plain
/// `SIGSEGV` instead of Rust's "has overflowed its stack" message. Nothing
/// in the handler allocates or takes a lock other than its own spin lock:
/// the zstd context is created up front, and single-shot decompression
/// into a buffer does not allocate. It saves and restores `errno`. ICU
/// never hands its data to a system call, which would get `EFAULT`
/// instead of a fault. Debuggers stop on these faults; in gdb use
/// `handle SIGSEGV nostop noprint pass`.
#[cfg(target_os = "linux")]
mod lazy {
    use super::{CHUNK, Packed};
    use std::cell::UnsafeCell;
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

    /// More than enough for the 10.9 MB package (84 chunks).
    const MAX_CHUNKS: usize = 4096;

    static BASE: AtomicUsize = AtomicUsize::new(0);
    static SPAN: AtomicUsize = AtomicUsize::new(0);
    static LOCK: AtomicBool = AtomicBool::new(false);
    static FILLED: [AtomicU8; MAX_CHUNKS] = [const { AtomicU8::new(0) }; MAX_CHUNKS];
    static PREV: OnceLock<libc::sigaction> = OnceLock::new();
    static STATE: OnceLock<State> = OnceLock::new();

    struct State {
        packed: Packed,
        /// Only used with [`LOCK`] held.
        dctx: UnsafeCell<zstd::bulk::Decompressor<'static>>,
    }

    // SAFETY: `dctx` is only touched by `fill`, with `LOCK` held, so never
    // from two threads at once; `packed` is read-only.
    unsafe impl Sync for State {}
    // SAFETY: as above; the decompressor owns plain heap memory.
    unsafe impl Send for State {}

    /// Reserve the package and install the fault handler. `None` if either
    /// fails (the caller decompresses eagerly instead).
    pub(super) fn map(packed: Packed) -> Option<&'static [u8]> {
        if packed.count > MAX_CHUNKS {
            return None;
        }
        let dctx = zstd::bulk::Decompressor::new().ok()?;
        let span = packed.count * CHUNK;
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
        if base == libc::MAP_FAILED {
            return None;
        }
        let state = State {
            packed,
            dctx: UnsafeCell::new(dctx),
        };
        if STATE.set(state).is_err() {
            return None;
        }
        // SAFETY: plain sigaction calls with zeroed, then filled structs.
        // The handler only acts on addresses inside the reservation, which
        // is published (BASE/SPAN) before the handler is installed.
        unsafe {
            let mut old: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(libc::SIGSEGV, std::ptr::null(), &mut old) != 0 {
                libc::munmap(base, span);
                return None;
            }
            let _ = PREV.set(old);
            SPAN.store(span, Ordering::Release);
            BASE.store(base as usize, Ordering::Release);
            let mut sa: libc::sigaction = std::mem::zeroed();
            sa.sa_sigaction = on_fault as *const () as libc::sighandler_t;
            sa.sa_flags = libc::SA_SIGINFO;
            libc::sigemptyset(&mut sa.sa_mask);
            if libc::sigaction(libc::SIGSEGV, &sa, std::ptr::null_mut()) != 0 {
                BASE.store(0, Ordering::Release);
                libc::munmap(base, span);
                return None;
            }
            Some(std::slice::from_raw_parts(base.cast::<u8>(), packed.len))
        }
    }

    extern "C" fn on_fault(sig: libc::c_int, info: *mut libc::siginfo_t, ctx: *mut libc::c_void) {
        // SAFETY: the kernel passes a valid siginfo for SIGSEGV.
        let addr = unsafe { (*info).si_addr() } as usize;
        let base = BASE.load(Ordering::Acquire);
        let span = SPAN.load(Ordering::Acquire);
        if base != 0 && addr >= base && addr < base + span {
            // SAFETY: errno is thread-local; restored before returning.
            let errno = unsafe { *libc::__errno_location() };
            let ok = fill((addr - base) / CHUNK, base);
            {
                let mut buf = *b"ICUPAGE 00000\n";
                let mut n = (addr - base) / CHUNK;
                for k in (8..13).rev() { buf[k] = b'0' + (n % 10) as u8; n /= 10; }
                unsafe { libc::write(2, buf.as_ptr().cast(), buf.len()); }
            }
            // SAFETY: as above.
            unsafe { *libc::__errno_location() = errno };
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
        let prev = PREV.get().copied();
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

    /// Make chunk `i` resident. Returns false if it cannot be.
    fn fill(i: usize, base: usize) -> bool {
        let Some(state) = STATE.get() else {
            return false;
        };
        if i >= state.packed.count {
            return false;
        }
        while LOCK
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            std::hint::spin_loop();
        }
        let ok = FILLED[i].load(Ordering::Acquire) == 1 || {
            // SAFETY: LOCK is held, so this is the only user of `dctx`.
            let dctx = unsafe { &mut *state.dctx.get() };
            // SAFETY: see `place`.
            let placed = unsafe { place(i, base, &state.packed, dctx) };
            if placed {
                FILLED[i].store(1, Ordering::Release);
            }
            placed
        };
        LOCK.store(false, Ordering::Release);
        ok
    }

    /// Decompress chunk `i` into a scratch mapping and move it over the
    /// reservation at `base + i * CHUNK`.
    ///
    /// # Safety
    /// `base` is the reservation made by `map`, `i` a chunk index of it, and
    /// the caller holds `LOCK`.
    unsafe fn place(
        i: usize,
        base: usize,
        packed: &Packed,
        dctx: &mut zstd::bulk::Decompressor<'static>,
    ) -> bool {
        // SAFETY: fresh private anonymous mapping, only used here.
        let scratch = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                CHUNK,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_PRIVATE | libc::MAP_ANONYMOUS,
                -1,
                0,
            )
        };
        if scratch == libc::MAP_FAILED {
            return false;
        }
        let len = packed.chunk_len(i);
        // SAFETY: `scratch` is CHUNK >= len writable bytes owned here.
        let out = unsafe { std::slice::from_raw_parts_mut(scratch.cast::<u8>(), len) };
        let ok = matches!(dctx.decompress_to_buffer(packed.frame(i), out), Ok(n) if n == len);
        // SAFETY: `scratch` is ours; on success it replaces the reserved
        // pages of chunk i (MREMAP_FIXED unmaps them in the same step);
        // ICU only ever reads the package, so it is made read-only first.
        unsafe {
            if !ok || libc::mprotect(scratch, CHUNK, libc::PROT_READ) != 0 {
                libc::munmap(scratch, CHUNK);
                return false;
            }
            let target = (base + i * CHUNK) as *mut libc::c_void;
            let moved = libc::mremap(
                scratch,
                CHUNK,
                CHUNK,
                libc::MREMAP_MAYMOVE | libc::MREMAP_FIXED,
                target,
            );
            if moved != target {
                libc::munmap(scratch, CHUNK);
                return false;
            }
        }
        true
    }

    /// Whether [`map`] succeeded.
    pub(super) fn active() -> bool {
        BASE.load(Ordering::Acquire) != 0
    }

    /// Chunks made resident so far.
    pub(super) fn resident() -> usize {
        FILLED
            .iter()
            .filter(|f| f.load(Ordering::Acquire) == 1)
            .count()
    }
}

/// How many [`CHUNK`]s of [`data`] are resident: `Some` when the lazy
/// path is in use (Linux), `None` when the package was decompressed at
/// once.
pub fn resident_chunks() -> Option<usize> {
    let _ = data();
    #[cfg(target_os = "linux")]
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
/// [`MEMBER`] (an ELF64 object), from symbol [`SYMBOL`] to the end of its
/// section. Used by the `icu_pack` example and the `icu_data` tests.
pub fn extract_from_archive(ar: &[u8]) -> Result<Vec<u8>, String> {
    let obj = archive_member(ar, MEMBER)?;
    elf_symbol_section_tail(obj, SYMBOL)
}

fn archive_member<'a>(ar: &'a [u8], name: &str) -> Result<&'a [u8], String> {
    if !ar.starts_with(b"!<arch>\n") {
        return Err("not an ar archive".into());
    }
    let mut pos = 8;
    let mut long_names: &[u8] = &[];
    while pos + 60 <= ar.len() {
        let hdr = &ar[pos..pos + 60];
        let size: usize = std::str::from_utf8(&hdr[48..58])
            .ok()
            .and_then(|s| s.trim().parse().ok())
            .ok_or("bad ar member size")?;
        let body = ar
            .get(pos + 60..pos + 60 + size)
            .ok_or("truncated ar member")?;
        let raw = std::str::from_utf8(&hdr[0..16])
            .map_err(|_| "bad ar name")?
            .trim_end();
        let member = if raw == "//" {
            long_names = body;
            None
        } else if let Some(off) = raw.strip_prefix('/').and_then(|o| o.parse::<usize>().ok()) {
            let rest = long_names.get(off..).ok_or("bad long name offset")?;
            let end = rest.iter().position(|&c| c == b'\n').unwrap_or(rest.len());
            Some(
                std::str::from_utf8(&rest[..end])
                    .map_err(|_| "bad long name")?
                    .trim_end_matches('/'),
            )
        } else {
            Some(raw.trim_end_matches('/'))
        };
        if member == Some(name) {
            return Ok(body);
        }
        pos += 60 + size + (size & 1);
    }
    Err(format!("{name} not found in archive"))
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
