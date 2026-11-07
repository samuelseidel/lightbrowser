//! ICU's break iterators and character classes, from the ICU that the
//! prebuilt V8 links (ICU 78, the version `icu_data` installs data for).
//! Blink's editing code finds word, sentence and grapheme boundaries with
//! these same ICU iterators (`WordBreakIterator`, `SentenceBreakIterator`)
//! and classifies characters with `u_ispunct`, `u_isalnum`, ..., so
//! `Selection.modify` (lb-webapi `rt/caret.rs`) uses them to stop where
//! Chrome stops, dictionary-based scripts included.
//!
//! The C API (`ubrk_*`, `u_*`) is in the V8 archive next to the C++ break
//! iterators V8 uses for `Intl.Segmenter`; naming it here pulls only the
//! small wrapper objects into the binary.

use std::ffi::{CString, c_char, c_int, c_void};

/// ICU's `UBreakIteratorType`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BreakKind {
    /// Grapheme clusters (`UBRK_CHARACTER`).
    Character,
    /// Words (`UBRK_WORD`).
    Word,
    /// Sentences (`UBRK_SENTENCE`).
    Sentence,
}

const UBRK_DONE: i32 = -1;

// SAFETY (whole block): these are ICU 78's C entry points, linked from the
// same `librusty_v8.a` V8 uses; the signatures follow `ubrk.h`/`uchar.h`
// (UBool is one byte, read as `u8`).
unsafe extern "C" {
    fn ubrk_open_78(
        kind: c_int,
        locale: *const c_char,
        text: *const u16,
        len: i32,
        status: *mut c_int,
    ) -> *mut c_void;
    fn ubrk_close_78(bi: *mut c_void);
    fn ubrk_following_78(bi: *mut c_void, offset: i32) -> i32;
    fn ubrk_preceding_78(bi: *mut c_void, offset: i32) -> i32;
    fn u_ispunct_78(c: i32) -> u8;
    fn u_isalnum_78(c: i32) -> u8;
    fn u_isprint_78(c: i32) -> u8;
    fn u_charType_78(c: i32) -> i8;
    fn u_charDirection_78(c: i32) -> c_int;
}

/// A break iterator over a UTF-16 text it owns.
pub struct Breaker {
    bi: *mut c_void,
    /// ICU keeps a pointer to the text: it lives as long as `bi`.
    text: Box<[u16]>,
}

impl Breaker {
    /// An iterator of `kind` for `locale` (BCP 47 or ICU form; empty for
    /// ICU's default) over `text`. `None` if ICU fails (no data).
    pub fn new(kind: BreakKind, locale: &str, text: &[u16]) -> Option<Breaker> {
        let text: Box<[u16]> = text.into();
        let loc = CString::new(locale.replace('-', "_")).ok()?;
        let kind = match kind {
            BreakKind::Character => 0,
            BreakKind::Word => 1,
            BreakKind::Sentence => 3,
        };
        let mut status: c_int = 0;
        // SAFETY: `loc` is NUL-terminated and `text` stays alive (and
        // unmoved: it is boxed) until `Drop` closes the iterator.
        let bi = unsafe {
            ubrk_open_78(
                kind,
                loc.as_ptr(),
                text.as_ptr(),
                i32::try_from(text.len()).ok()?,
                &mut status,
            )
        };
        if bi.is_null() || status > 0 {
            if !bi.is_null() {
                // SAFETY: `bi` came from `ubrk_open` and is closed once.
                unsafe { ubrk_close_78(bi) };
            }
            return None;
        }
        Some(Breaker { bi, text })
    }

    /// The text's length in UTF-16 units.
    pub fn len(&self) -> usize {
        self.text.len()
    }

    /// Whether the text is empty.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }

    /// The first boundary after `offset` (`ubrk_following`).
    pub fn following(&mut self, offset: usize) -> Option<usize> {
        let o = i32::try_from(offset.min(self.text.len())).ok()?;
        // SAFETY: `bi` is open; ICU clamps out-of-range offsets.
        let r = unsafe { ubrk_following_78(self.bi, o) };
        (r != UBRK_DONE).then_some(r as usize)
    }

    /// The last boundary before `offset` (`ubrk_preceding`).
    pub fn preceding(&mut self, offset: usize) -> Option<usize> {
        let o = i32::try_from(offset.min(self.text.len())).ok()?;
        // SAFETY: as in `following`.
        let r = unsafe { ubrk_preceding_78(self.bi, o) };
        (r != UBRK_DONE).then_some(r as usize)
    }
}

impl Drop for Breaker {
    fn drop(&mut self) {
        // SAFETY: `bi` came from `ubrk_open` and is closed exactly once.
        unsafe { ubrk_close_78(self.bi) };
    }
}

/// `u_ispunct`.
pub fn is_punct(c: u32) -> bool {
    // SAFETY: pure lookup on a code point.
    unsafe { u_ispunct_78(c as i32) != 0 }
}

/// `u_isalnum`.
pub fn is_alnum(c: u32) -> bool {
    // SAFETY: pure lookup on a code point.
    unsafe { u_isalnum_78(c as i32) != 0 }
}

/// `u_isprint`.
pub fn is_print(c: u32) -> bool {
    // SAFETY: pure lookup on a code point.
    unsafe { u_isprint_78(c as i32) != 0 }
}

/// `u_charType` (`UCharCategory`: 25 to 28 are the symbol categories Sm,
/// Sc, Sk, So).
pub fn char_type(c: u32) -> i8 {
    // SAFETY: pure lookup on a code point.
    unsafe { u_charType_78(c as i32) }
}

/// `u_charDirection` is `U_WHITE_SPACE_NEUTRAL` (9).
pub fn is_white_space_neutral(c: u32) -> bool {
    // SAFETY: pure lookup on a code point.
    unsafe { u_charDirection_78(c as i32) == 9 }
}
