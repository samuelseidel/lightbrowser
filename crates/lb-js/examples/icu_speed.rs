//! Scratch probe (temporary): ICU data size and decode time when compressed in independent chunks.
use std::time::Instant;
fn main() {
    let raw = lb_js::icu_data::data().to_vec();
    println!("raw {} shipped {}", raw.len(), lb_js::icu_data::COMPRESSED.len());
    for chunk in [16384usize, 32768, 65536, 131072, 262144] {
        for lvl in [19, 22] {
            let mut total = 0;
            let mut parts = Vec::new();
            for c in raw.chunks(chunk) {
                let mut comp = zstd::bulk::Compressor::new(lvl).unwrap();
                let z = comp.compress(c).unwrap();
                total += z.len() + 4;
                parts.push(z);
            }
            let mut out = vec![0u8; chunk];
            let t = Instant::now();
            let mut d = zstd::bulk::Decompressor::new().unwrap();
            for z in &parts { d.decompress_to_buffer(z, &mut out[..]).unwrap(); }
            println!("chunk {chunk:6} level {lvl}: {total} bytes (+{}), full decode {:.2} ms, per chunk {:.1} us", total as i64 - lb_js::icu_data::COMPRESSED.len() as i64, t.elapsed().as_secs_f64()*1e3, t.elapsed().as_secs_f64()*1e6/parts.len() as f64);
        }
    }
}
