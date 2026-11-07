//! Minimal HTTP/2 reader for the client side of a connection preface:
//! frames, SETTINGS, WINDOW_UPDATE, PRIORITY and HPACK header *names*.
//!
//! HPACK values are skipped, not decoded. Names that are Huffman-coded
//! literals come back as `None`; pseudo-headers always use the static table.

pub const PREFACE: &[u8] = b"PRI * HTTP/2.0\r\n\r\nSM\r\n\r\n";

pub const DATA: u8 = 0x0;
pub const HEADERS: u8 = 0x1;
pub const PRIORITY: u8 = 0x2;
pub const SETTINGS: u8 = 0x4;
pub const PING: u8 = 0x6;
pub const GOAWAY: u8 = 0x7;
pub const WINDOW_UPDATE: u8 = 0x8;
pub const CONTINUATION: u8 = 0x9;

pub const FLAG_ACK: u8 = 0x1;
pub const FLAG_END_STREAM: u8 = 0x1;
pub const FLAG_END_HEADERS: u8 = 0x4;
pub const FLAG_PADDED: u8 = 0x8;
pub const FLAG_PRIORITY: u8 = 0x20;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Frame {
    pub kind: u8,
    pub flags: u8,
    pub stream: u32,
    pub payload: Vec<u8>,
}

impl Frame {
    pub fn new(kind: u8, flags: u8, stream: u32, payload: Vec<u8>) -> Self {
        Self {
            kind,
            flags,
            stream,
            payload,
        }
    }

    /// Parses one frame from the front of `buf`. `None` if incomplete.
    pub fn parse(buf: &[u8]) -> Option<(Self, usize)> {
        if buf.len() < 9 {
            return None;
        }
        let len = u32::from_be_bytes([0, buf[0], buf[1], buf[2]]) as usize;
        if buf.len() < 9 + len {
            return None;
        }
        let stream = u32::from_be_bytes([buf[5], buf[6], buf[7], buf[8]]) & 0x7fff_ffff;
        let f = Self::new(buf[3], buf[4], stream, buf[9..9 + len].to_vec());
        Some((f, 9 + len))
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut o = (self.payload.len() as u32).to_be_bytes()[1..].to_vec();
        o.push(self.kind);
        o.push(self.flags);
        o.extend_from_slice(&self.stream.to_be_bytes());
        o.extend_from_slice(&self.payload);
        o
    }
}

/// SETTINGS payload as `(id, value)` in wire order.
pub fn parse_settings(p: &[u8]) -> Vec<(u16, u32)> {
    p.as_chunks::<6>()
        .0
        .iter()
        .map(|c| {
            (
                u16::from_be_bytes([c[0], c[1]]),
                u32::from_be_bytes([c[2], c[3], c[4], c[5]]),
            )
        })
        .collect()
}

/// RFC 9113 / RFC 8441 / RFC 9218 setting name.
pub fn setting_name(id: u16) -> Option<&'static str> {
    Some(match id {
        0x1 => "HEADER_TABLE_SIZE",
        0x2 => "ENABLE_PUSH",
        0x3 => "MAX_CONCURRENT_STREAMS",
        0x4 => "INITIAL_WINDOW_SIZE",
        0x5 => "MAX_FRAME_SIZE",
        0x6 => "MAX_HEADER_LIST_SIZE",
        0x8 => "ENABLE_CONNECT_PROTOCOL",
        0x9 => "NO_RFC7540_PRIORITIES",
        _ => return None,
    })
}

pub fn parse_window_update(p: &[u8]) -> Option<u32> {
    (p.len() == 4).then(|| u32::from_be_bytes([p[0], p[1], p[2], p[3]]) & 0x7fff_ffff)
}

/// Stream dependency, from a PRIORITY frame or a HEADERS frame's priority
/// block. `weight` is the wire byte (real weight minus one).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Priority {
    pub stream: u32,
    pub exclusive: bool,
    pub depends_on: u32,
    pub weight: u8,
}

pub fn parse_priority(stream: u32, p: &[u8]) -> Option<Priority> {
    if p.len() < 5 {
        return None;
    }
    let dep = u32::from_be_bytes([p[0], p[1], p[2], p[3]]);
    Some(Priority {
        stream,
        exclusive: dep & 0x8000_0000 != 0,
        depends_on: dep & 0x7fff_ffff,
        weight: p[4],
    })
}

/// Splits a HEADERS payload into its optional priority block and the header
/// block fragment (padding removed).
pub fn headers_fragment(f: &Frame) -> Option<(Option<Priority>, &[u8])> {
    let mut p = &f.payload[..];
    let mut pad = 0usize;
    if f.flags & FLAG_PADDED != 0 {
        pad = *p.first()? as usize;
        p = &p[1..];
    }
    let mut prio = None;
    if f.flags & FLAG_PRIORITY != 0 {
        prio = Some(parse_priority(f.stream, p)?);
        p = &p[5..];
    }
    let end = p.len().checked_sub(pad)?;
    Some((prio, &p[..end]))
}

const STATIC_NAMES: [&str; 61] = [
    ":authority",
    ":method",
    ":method",
    ":path",
    ":path",
    ":scheme",
    ":scheme",
    ":status",
    ":status",
    ":status",
    ":status",
    ":status",
    ":status",
    ":status",
    "accept-charset",
    "accept-encoding",
    "accept-language",
    "accept-ranges",
    "accept",
    "access-control-allow-origin",
    "age",
    "allow",
    "authorization",
    "cache-control",
    "content-disposition",
    "content-encoding",
    "content-language",
    "content-length",
    "content-location",
    "content-range",
    "content-type",
    "cookie",
    "date",
    "etag",
    "expect",
    "expires",
    "from",
    "host",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-range",
    "if-unmodified-since",
    "last-modified",
    "link",
    "location",
    "max-forwards",
    "proxy-authenticate",
    "proxy-authorization",
    "range",
    "referer",
    "refresh",
    "retry-after",
    "server",
    "set-cookie",
    "strict-transport-security",
    "transfer-encoding",
    "user-agent",
    "vary",
    "via",
    "www-authenticate",
];

/// HPACK prefixed integer (RFC 7541 5.1).
fn hpack_int(b: &[u8], pos: &mut usize, prefix: u8) -> Option<usize> {
    let mask = (1u16 << prefix) as usize - 1;
    let mut v = (*b.get(*pos)? as usize) & mask;
    *pos += 1;
    if v < mask {
        return Some(v);
    }
    let mut shift = 0;
    loop {
        let byte = *b.get(*pos)?;
        *pos += 1;
        v = v.checked_add(((byte & 0x7f) as usize).checked_shl(shift)?)?;
        shift += 7;
        if byte & 0x80 == 0 {
            return Some(v);
        }
        if shift > 28 {
            return None;
        }
    }
}

/// HPACK string; `None` inside means Huffman-coded (not decoded here).
fn hpack_str(b: &[u8], pos: &mut usize) -> Option<Option<String>> {
    let huff = *b.get(*pos)? & 0x80 != 0;
    let len = hpack_int(b, pos, 7)?;
    let s = b.get(*pos..*pos + len)?;
    *pos += len;
    Some((!huff).then(|| String::from_utf8_lossy(s).into_owned()))
}

/// Header names of one header block, in order. Tracks the dynamic table
/// names so indexed references to fresh entries resolve.
pub fn hpack_names(block: &[u8]) -> Option<Vec<Option<String>>> {
    let mut dynamic: Vec<Option<String>> = Vec::new(); // newest first
    let lookup = |dynamic: &Vec<Option<String>>, i: usize| -> Option<Option<String>> {
        match i {
            0 => None,
            1..=61 => Some(Some(STATIC_NAMES[i - 1].to_owned())),
            _ => Some(dynamic.get(i - 62).cloned().unwrap_or(None)),
        }
    };
    let mut out = Vec::new();
    let mut pos = 0;
    while pos < block.len() {
        let b = block[pos];
        if b & 0x80 != 0 {
            let i = hpack_int(block, &mut pos, 7)?;
            out.push(lookup(&dynamic, i)?);
        } else if b & 0xe0 == 0x20 {
            hpack_int(block, &mut pos, 5)?; // table size update
        } else {
            let (prefix, indexing) = if b & 0xc0 == 0x40 {
                (6, true)
            } else {
                (4, false)
            };
            let i = hpack_int(block, &mut pos, prefix)?;
            let name = if i == 0 {
                hpack_str(block, &mut pos)?
            } else {
                lookup(&dynamic, i)?
            };
            hpack_str(block, &mut pos)?; // value, skipped
            if indexing {
                dynamic.insert(0, name.clone());
            }
            out.push(name);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_round_trip() {
        let f = Frame::new(SETTINGS, 0, 0, vec![0, 1, 0, 1, 0, 0, 0, 2, 0, 0, 0, 0]);
        let enc = f.encode();
        assert_eq!(Frame::parse(&enc), Some((f.clone(), enc.len())));
        assert_eq!(Frame::parse(&enc[..enc.len() - 1]), None);
        assert_eq!(parse_settings(&f.payload), vec![(1, 65536), (2, 0)]);
    }

    #[test]
    fn hpack_chrome_style_pseudo_headers() {
        // :method GET (idx 2), :authority literal-incremental (idx 1),
        // :scheme https (idx 7), :path / (idx 4), then a raw literal name.
        let mut block = vec![0x82, 0x41, 0x09];
        block.extend_from_slice(b"localhost");
        block.extend_from_slice(&[0x87, 0x84, 0x40, 0x03]);
        block.extend_from_slice(b"foo");
        block.extend_from_slice(&[0x81, 0xff]); // 1-byte Huffman value, skipped
        block.push(0xbe); // index 62 = newest dynamic entry ("foo")
        let names = hpack_names(&block).unwrap();
        let names: Vec<String> = names.into_iter().map(|n| n.unwrap()).collect();
        assert_eq!(
            names,
            [":method", ":authority", ":scheme", ":path", "foo", "foo"]
        );
    }

    #[test]
    fn headers_with_priority_and_padding() {
        let mut payload = vec![2]; // pad length
        payload.extend_from_slice(&0x8000_0000u32.to_be_bytes());
        payload.push(255);
        payload.extend_from_slice(&[0x82, 0x84]);
        payload.extend_from_slice(&[0, 0]);
        let f = Frame::new(
            HEADERS,
            FLAG_PADDED | FLAG_PRIORITY | FLAG_END_HEADERS,
            1,
            payload,
        );
        let (prio, frag) = headers_fragment(&f).unwrap();
        let prio = prio.unwrap();
        assert!(prio.exclusive);
        assert_eq!((prio.depends_on, prio.weight), (0, 255));
        assert_eq!(frag, &[0x82, 0x84]);
    }

    #[test]
    fn hpack_multibyte_int() {
        let b = [0x1f, 0x9a, 0x0a];
        let mut pos = 0;
        assert_eq!(hpack_int(&b, &mut pos, 5), Some(1337));
        assert_eq!(pos, 3);
    }
}
