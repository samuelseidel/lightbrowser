//! Chrome-shaped ids: 32 upper-case hex digits for targets, browser contexts,
//! sessions and loaders. Unique per process; not cryptographic.

use std::cell::Cell;
use std::time::{SystemTime, UNIX_EPOCH};

/// SplitMix64 over a counter, seeded from the clock and the address of the
/// generator, so two processes do not hand out the same ids.
#[derive(Debug)]
pub struct IdGen {
    state: Cell<u64>,
}

impl Default for IdGen {
    fn default() -> Self {
        Self::new()
    }
}

impl IdGen {
    pub fn new() -> IdGen {
        let t = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0);
        let g = IdGen {
            state: Cell::new(0),
        };
        g.state.set(t ^ (&g as *const IdGen as u64).rotate_left(32));
        g
    }

    /// A generator with a fixed seed (tests).
    pub fn seeded(seed: u64) -> IdGen {
        IdGen {
            state: Cell::new(seed),
        }
    }

    fn next_u64(&self) -> u64 {
        let s = self.state.get().wrapping_add(0x9E37_79B9_7F4A_7C15);
        self.state.set(s);
        let mut z = s;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// 32 upper-case hex digits, like Chrome's target and session ids.
    pub fn hex32(&self) -> String {
        format!("{:016X}{:016X}", self.next_u64(), self.next_u64())
    }

    /// A lower-case UUID (`webSocketDebuggerUrl` path).
    pub fn uuid(&self) -> String {
        let a = self.next_u64();
        let b = self.next_u64();
        format!(
            "{:08x}-{:04x}-4{:03x}-{:04x}-{:012x}",
            a >> 32,
            (a >> 16) & 0xffff,
            a & 0x0fff,
            ((b >> 48) & 0x3fff) | 0x8000,
            b & 0xffff_ffff_ffff
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_chrome_shape() {
        let g = IdGen::seeded(1);
        let a = g.hex32();
        assert_eq!(a.len(), 32);
        assert!(
            a.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
        );
        assert_ne!(a, g.hex32());
        let u = g.uuid();
        assert_eq!(u.len(), 36);
        assert_eq!(&u[14..15], "4");
    }
}
