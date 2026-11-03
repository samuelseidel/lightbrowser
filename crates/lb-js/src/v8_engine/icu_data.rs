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
//! [`install`] decompresses them once per process, before V8 is
//! initialised, and registers them with `udata_setCommonData`. ICU searches
//! registered packages in order, so every item comes from that copy; the
//! empty linked-in package is only consulted for items that exist in
//! neither, exactly as the full one was for items it lacked. The data is
//! byte-identical to the archive's (`icu_data` tests in `tests/v8.rs`), so
//! Intl behaviour is unchanged.
//!
//! Cost, paid when the first engine starts (not at process start): one
//! 10.9 MB heap allocation, all resident, where the linked-in data was
//! paged in from the binary only as far as ICU read it; and ~15 ms to
//! decompress (release). The compressed pages are dropped again.

use std::sync::OnceLock;

/// Name of the data symbol in the prebuilt archive (ICU major version 78,
/// matching `v8::icu::set_common_data_78`).
pub const SYMBOL: &str = "icudt78_dat";

/// Archive member that holds the data.
pub const MEMBER: &str = "icudtl_dat.o";

/// The archive's ICU data, zstd-compressed (level 22, 16 MiB window).
pub const COMPRESSED: &[u8] = include_bytes!("../../icu/icudt78.dat.zst");

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

/// The decompressed ICU data (decompressed on first call, then kept for
/// the life of the process, as ICU requires). 16-byte aligned.
pub fn data() -> &'static [u8] {
    DATA.get_or_init(|| {
        let len = zstd::zstd_safe::get_frame_content_size(COMPRESSED)
            .ok()
            .flatten()
            .expect("ICU data frame has a content size") as usize;
        // ICU wants the package 16-byte aligned: back it with u128s.
        let mut words = vec![0u128; len.div_ceil(16)].into_boxed_slice();
        // SAFETY: `words` owns `len.div_ceil(16) * 16 >= len` initialised
        // bytes, and u8 has no alignment or validity requirements.
        let bytes = unsafe { std::slice::from_raw_parts_mut(words.as_mut_ptr().cast::<u8>(), len) };
        let n = zstd::bulk::Decompressor::new()
            .and_then(|mut d| d.decompress_to_buffer(COMPRESSED, bytes))
            .expect("ICU data decompresses");
        assert_eq!(n, len, "ICU data length");
        release_compressed_pages();
        let words: &'static [u128] = Box::leak(words);
        // SAFETY: as above; the leaked allocation lives for the process.
        unsafe { std::slice::from_raw_parts(words.as_ptr().cast::<u8>(), len) }
    })
}

/// Drops the pages of [`COMPRESSED`] we just read from the resident set
/// (3.7 MB). They are clean, read-only pages of the binary's file
/// mapping, so the kernel can drop them; a later read (only the tests do
/// one) faults them back in from the file.
fn release_compressed_pages() {
    #[cfg(target_os = "linux")]
    {
        const PAGE: usize = 4096;
        let start = COMPRESSED.as_ptr() as usize;
        let first = start.div_ceil(PAGE) * PAGE;
        let end = (start + COMPRESSED.len()) / PAGE * PAGE;
        if end > first {
            // SAFETY: the range lies inside COMPRESSED, read-only data of
            // the executable's private file mapping; MADV_DONTNEED only
            // drops resident pages, which are re-read from the file on the
            // next access. No Rust reference sees different contents.
            unsafe {
                libc::madvise(first as *mut libc::c_void, end - first, libc::MADV_DONTNEED);
            }
        }
    }
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
