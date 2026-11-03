//! Parses an indirect object (`N G obj … endobj`, with any stream) from arbitrary bytes, as a
//! damaged or hostile PDF presents one. The first two bytes pick the offset, the third whether
//! an indirect `/Length` resolves and to what.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdfcraft_cos::parse_indirect;

fuzz_target!(|data: &[u8]| {
    let Some((head, body)) = data.split_first_chunk::<3>() else { return };
    let offset = usize::from(u16::from_le_bytes([head[0], head[1]])) % (body.len() + 1);
    let length = (head[2] & 0x80 != 0).then(|| i64::from(head[2] & 0x7f) * 37 - 100);
    let _ = parse_indirect(body, offset, &|_| length);
});
