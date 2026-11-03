//! Parses an arbitrary content stream, writes its operators back and parses them again. Parsing
//! must not panic, and once written the operators must survive every further round trip
//! unchanged (the first round may normalise numbers and spacing). Streams with an inline image
//! whose end is a guess are left out of the round trip.
#![no_main]

use libfuzzer_sys::fuzz_target;
use pdfcraft_content::{Op, ops, parse, serialize_ops};

/// The operators without `span`, which locates each one in its own round's bytes and so moves
/// whenever the first round normalises an earlier operand.
fn unplaced(ops: Vec<Op>) -> Vec<Op> {
    ops.into_iter().map(|op| Op { span: 0..0, ..op }).collect()
}

fuzz_target!(|data: &[u8]| {
    let first = parse(data);
    // The streaming reader sees the same operators as the whole-stream parser.
    assert_eq!(ops(data).collect::<Vec<_>>(), first.ops);
    if first.ambiguous_images != 0 {
        return;
    }
    let second = parse(&serialize_ops(&first.ops));
    let third = parse(&serialize_ops(&second.ops));
    assert_eq!(unplaced(second.ops), unplaced(third.ops));
});
