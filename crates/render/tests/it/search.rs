use pdfcraft_render::{PageText, TextGlyph, text::fold_case};
use std::ops::Range;
use unicode_normalization::UnicodeNormalization;

fn page(parts: &[&str]) -> PageText {
    PageText {
        glyphs: parts.iter().map(|text| TextGlyph { text: (*text).into(), rect: [0.0; 4], quad: [[0.0; 2]; 4], direction: [1.0, 0.0] }).collect(),
        line_of: vec![0; parts.len()],
        space_before: vec![false; parts.len()],
    }
}

// The original straightforward search is an independent oracle for valid page metadata.
fn reference(page: &PageText, query: &str, sensitive: bool, whole: bool, limit: usize) -> Vec<Range<usize>> {
    let fold = |s: &str| if sensitive { s.to_owned() } else { fold_case(s) };
    let needle: Vec<char> = fold(query).split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    if needle.is_empty() || limit == 0 {
        return Vec::new();
    }
    let mut chars = Vec::new();
    for (i, glyph) in page.glyphs.iter().enumerate() {
        if i > 0 && (page.space_before[i] || page.line_of[i] != page.line_of[i - 1]) && chars.last().is_some_and(|&(c, _)| c != ' ') {
            chars.push((' ', i));
        }
        for c in fold(&glyph.text).chars() {
            let c = if c.is_whitespace() { ' ' } else { c };
            if c != ' ' || chars.last().is_none_or(|&(last, _)| last != ' ') {
                chars.push((c, i));
            }
        }
    }
    let word = |i: Option<usize>| i.and_then(|i| chars.get(i)).is_some_and(|&(c, _)| c.is_alphanumeric());
    let mut hits = Vec::new();
    let mut start = 0;
    while start + needle.len() <= chars.len() {
        let end = start + needle.len();
        if (!whole || (!word(start.checked_sub(1)) && !word(Some(end)))) && chars[start..end].iter().map(|&(c, _)| c).eq(needle.iter().copied()) {
            hits.push(chars[start].1..chars[end - 1].1 + 1);
            if hits.len() == limit {
                break;
            }
            start = end;
        } else {
            start += 1;
        }
    }
    hits
}

#[test]
fn search_tolerates_missing_layout_metadata() {
    let mut text = page(&["a", "b", "c"]);
    text.line_of.clear();
    text.space_before.clear();
    assert_eq!(text.find("abc"), vec![0..3]);
    text.space_before = vec![false, true];
    assert_eq!(text.find("a bc"), vec![0..3]);
    text.line_of = vec![0, 1];
    assert_eq!(text.find("a bc"), vec![0..3]);
}

#[test]
fn whole_word_rejection_keeps_overlapping_candidates() {
    let text = page(&["xaa-aa-aa"]);
    assert_eq!(text.find_opts("aa-aa", true, true), vec![0..1]);
    // Accepted matches still do not overlap, even within one multi-character glyph.
    assert_eq!(page(&["aaaaa"]).find("aaa"), vec![0..1]);
    assert_eq!(page(&["aaaaa"]).find("aa"), vec![0..1, 0..1]);
}

#[test]
fn search_matches_reference_for_unicode_gaps_limits_and_word_boundaries() {
    let alphabet = ["a", "b", " ", "-", "İ", "ı", "ς", "Σ", "\t", "\n", "ffi", "", "界", "e\u{301}", "العربية"];
    let mut seed = 1234567u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 32) as usize
    };
    for case in 0..2000 {
        let parts: Vec<_> = (0..10 + case % 70).map(|_| alphabet[next() % alphabet.len()]).collect();
        let query = if case % 2 == 0 {
            parts.iter().skip(case % 8).take(1 + case % 9).copied().collect::<String>()
        } else {
            (0..1 + case % 9).map(|_| alphabet[next() % alphabet.len()]).collect::<String>()
        };
        let mut text = page(&parts);
        for i in 1..parts.len() {
            text.space_before[i] = next() % 11 == 0;
            text.line_of[i] = text.line_of[i - 1] + u32::from(next() % 17 == 0);
        }
        for sensitive in [false, true] {
            for whole in [false, true] {
                for limit in [0, 1, 2, usize::MAX] {
                    assert_eq!(
                        text.find_opts_limited(&query, sensitive, whole, limit),
                        reference(&text, &query, sensitive, whole, limit),
                        "case {case}: {query:?}, sensitive={sensitive}, whole={whole}, limit={limit}"
                    );
                }
            }
        }
    }
}

/// A query and text that must find each other, whichever is typed and in either case mode.
fn assert_same_letters(a: &str, b: &str) {
    for sensitive in [false, true] {
        assert_eq!(page(&[a]).find_opts(b, sensitive, false), vec![0..1], "{b:?} in {a:?}, sensitive={sensitive}");
        assert_eq!(page(&[b]).find_opts(a, sensitive, false), vec![0..1], "{a:?} in {b:?}, sensitive={sensitive}");
    }
}

#[test]
fn kangxi_radicals_match_their_ideographs() {
    assert_same_letters("\u{2F92}", "見"); // ⾒
    assert_same_letters("\u{2F00}", "一"); // first in the block
    assert_same_letters("\u{2FD5}", "龠"); // last in the block
    for c in (0x2F00..=0x2FD5).filter_map(char::from_u32) {
        let ideograph: String = c.nfkc().collect();
        assert_ne!(ideograph, c.to_string());
        assert_same_letters(&c.to_string(), &ideograph);
    }
    // The radicals supplement: the two forms Unicode maps, and only those.
    assert_same_letters("\u{2E9F}", "母");
    assert_same_letters("\u{2EF3}", "龟");
    assert!(page(&["\u{2E80}"]).find("\u{2E81}").is_empty());
    assert_eq!(page(&["\u{2E80}"]).find("\u{2E80}"), vec![0..1]);
}

#[test]
fn arabic_presentation_forms_match_base_letters() {
    assert_same_letters("\u{FEB3}", "س"); // initial seen
    assert_same_letters("\u{FEF2}", "ي"); // final yeh
    assert_same_letters("\u{FB8E}", "ك"); // isolated Persian kaf, through ک
    assert_same_letters("\u{FE81}", "آ"); // alef with madda stays composed
    assert_same_letters("\u{FB50}", "\u{671}"); // start of the first block
    assert_same_letters("\u{FEFC}", "لا"); // end of the second block's letters
    // Ligatures with marks are their letters, so the word is found as it is typed.
    assert_same_letters("\u{FC5B}", "ذ");
    assert_eq!(page(&["\u{FC5B}", "ل", "ك"]).find("ذلك"), vec![0..3]);
    // A form of marks alone has no letter, so it matches its mark.
    assert_same_letters("\u{FE70}", "\u{64B}");
    // Every code point in the two blocks still finds itself, assigned or not.
    for c in (0xFB50..=0xFDFF).chain(0xFE70..=0xFEFF).filter_map(char::from_u32) {
        let s = c.to_string();
        assert_eq!(page(&[s.as_str()]).find(&s), vec![0..1], "U+{:04X}", u32::from(c));
    }
}

#[test]
fn arabic_and_persian_letter_variants_match() {
    for yeh in ["\u{64A}", "\u{6CC}", "\u{649}"] {
        assert_same_letters(yeh, "\u{64A}");
        assert_same_letters(yeh, "\u{6CC}");
        assert_same_letters(yeh, "\u{649}");
    }
    assert_same_letters("\u{643}", "\u{6A9}");
    assert_eq!(page(&["ک", "ت", "ا", "ب"]).find("كتاب"), vec![0..4]);
    // Other letters stay apart.
    assert!(page(&["\u{64A}"]).find("\u{643}").is_empty());
    assert!(page(&["\u{627}"]).find("\u{649}").is_empty());
}

#[test]
fn arabic_indic_digits_match_ascii() {
    for (i, ascii) in ('0'..='9').enumerate() {
        let offset = u32::try_from(i).unwrap();
        for digit in [0x660, 0x6F0].map(|base| char::from_u32(base + offset).unwrap()) {
            assert_same_letters(&digit.to_string(), &ascii.to_string());
        }
    }
    assert_eq!(page(&["۱", "۲", "۳"]).find("123"), vec![0..3]);
    assert_eq!(page(&["1", "2", "3"]).find("١٢٣"), vec![0..3]);
    // Digits are still word characters, so the word boundary holds across the scripts.
    assert!(page(&["۱", "۲", "۳"]).find_opts("12", false, true).is_empty());
    assert_eq!(page(&["۱", "۲", "۳"]).find_opts("123", false, true), vec![0..3]);
}

/// The boxes of a row of glyphs, each 10 points wide, so a wrong glyph range shows as a wrong box.
fn laid_out(parts: &[&str]) -> PageText {
    let mut text = page(parts);
    for (i, glyph) in text.glyphs.iter_mut().enumerate() {
        let x0 = 10.0 * i as f32;
        glyph.rect = [x0, 0.0, x0 + 10.0, 10.0];
        glyph.quad = [[x0, 0.0], [x0 + 10.0, 0.0], [x0 + 10.0, 10.0], [x0, 10.0]];
    }
    text
}

#[test]
fn matches_map_to_the_right_glyphs_through_expansions() {
    // ﻻ is 2 letters, ﷲ is 4 and ﷺ is 4 words with spaces in one glyph.
    let text = laid_out(&["x", "\u{FEFB}", "y", "\u{FDF2}", "z", "\u{FDFA}", "w"]);
    for (query, hit) in [
        ("x", 0..1),
        ("لا", 1..2),
        ("ل", 1..2),
        ("xل", 0..2),
        ("لاy", 1..3),
        ("y", 2..3),
        ("الله", 3..4),
        ("yالله", 2..4),
        ("اللهz", 3..5),
        ("z", 4..5),
        ("عليه", 5..6),
        ("وسلمw", 5..7),
        ("w", 6..7),
    ] {
        assert_eq!(text.find_opts_limited(query, false, false, 1), vec![hit.clone()], "{query}");
        assert_eq!(text.line_rects(hit.clone()), vec![[10.0 * hit.start as f32, 0.0, 10.0 * hit.end as f32, 10.0]], "{query}");
    }
    // The text reported for a match stays the document's own.
    assert_eq!(text.text_of(1..3), "\u{FEFB}y");
    // Words inside an expansion have word boundaries of their own.
    assert_eq!(page(&["\u{FDFA}"]).find_opts("عليه", false, true), vec![0..1]);
    assert!(page(&["\u{FDFA}"]).find_opts("علي", false, true).is_empty());
}

#[test]
fn match_counts_stay_right_through_expansions() {
    let text = page(&["\u{FEFB}", "\u{FEFB}", "ل", "ا"]);
    assert_eq!(text.find("لا"), vec![0..1, 1..2, 2..4]);
    assert_eq!(text.find("\u{FEFB}"), vec![0..1, 1..2, 2..4]);
    // A match ending inside one glyph and starting the next spans both, without overlap.
    assert_eq!(text.find("ال"), vec![0..2, 1..3]);
    assert_eq!(text.find_opts_limited("لا", false, false, 2), vec![0..1, 1..2]);
    // Two matches inside one glyph count as 2, as they do for any multi-character glyph.
    assert_eq!(page(&["\u{FDF2}"]).find("ل"), vec![0..1, 0..1]);
}

#[test]
fn latin_search_is_not_folded() {
    // Normalising the whole text would break each of these.
    assert_eq!(page(&["e\u{301}"]).find("e"), vec![0..1]);
    assert!(page(&["\u{E9}"]).find("e").is_empty());
    assert!(page(&["\u{FB01}"]).find("fi").is_empty());
    assert!(page(&["\u{BD}"]).find("1").is_empty());
    assert!(page(&["\u{2122}"]).find("tm").is_empty());
    assert!(page(&["\u{B2}"]).find("2").is_empty());
    // Latin digits and case behave as before.
    assert_eq!(page(&["A", "1"]).find_opts("a1", false, false), vec![0..2]);
    assert!(page(&["A", "1"]).find_opts("a1", true, false).is_empty());
}

#[test]
fn redaction_search_finds_the_literal_matches_folding_would_lose() {
    // Folded, the first match takes the variant digit and would leave the last glyph unmarked.
    let shifted = page(&["۱", "1", "1"]);
    assert_eq!(shifted.find("11"), vec![0..2]);
    assert_eq!(shifted.find_for_redaction("11", false, false), vec![0..2, 1..3]);
    // Folded, the radical becomes a letter and closes the word boundary.
    let beside = page(&["\u{2F92}", "見"]);
    assert!(beside.find_opts("見", false, true).is_empty());
    assert_eq!(beside.find_for_redaction("見", false, true), vec![1..2]);
    // Folding still adds what the literal text alone wouldn't find.
    assert_eq!(page(&["\u{2F92}", " ", "۱"]).find_for_redaction("見", false, false), vec![0..1]);
    assert_eq!(page(&["\u{2F92}", " ", "۱"]).find_for_redaction("1", false, false), vec![2..3]);
    // Nothing is counted twice, and a document with no variants is marked as it always was.
    assert_eq!(page(&["a", "b", "a", "b"]).find_for_redaction("ab", false, false), vec![0..2, 2..4]);
    assert_eq!(page(&["aaaaa"]).find_for_redaction("aa", false, false), vec![0..1, 0..1]);
}

#[test]
fn redaction_search_marks_everything_the_literal_search_did() {
    let alphabet = ["a", "1", "۱", "١", " ", "-", "見", "\u{2F92}", "\u{FEFB}", "ل", "ا", "ی", "ي", "ى", "ک", "ك", "\u{FDF2}", "\u{FE70}", "İ", "\n"];
    let mut seed = 7654321u64;
    let mut next = || {
        seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
        (seed >> 32) as usize
    };
    for case in 0..2000 {
        let parts: Vec<_> = (0..5 + case % 30).map(|_| alphabet[next() % alphabet.len()]).collect();
        let query: String = (0..1 + case % 4).map(|_| alphabet[next() % alphabet.len()]).collect();
        let mut text = page(&parts);
        for i in 1..parts.len() {
            text.space_before[i] = next() % 11 == 0;
            text.line_of[i] = text.line_of[i - 1] + u32::from(next() % 17 == 0);
        }
        for sensitive in [false, true] {
            for whole in [false, true] {
                let marked = text.find_for_redaction(&query, sensitive, whole);
                for hit in reference(&text, &query, sensitive, whole, usize::MAX) {
                    assert!(marked.contains(&hit), "case {case}: {query:?}, {hit:?} lost, sensitive={sensitive}, whole={whole}");
                }
            }
        }
    }
}

#[test]
fn long_repeated_prefix_search_preserves_the_last_match_and_limit() {
    let mut parts = vec!["a"; 131072];
    parts.push("b");
    let text = page(&parts);
    let query = format!("{}b", "a".repeat(4095));
    assert_eq!(text.find_opts_limited(&query, false, false, 1), vec![126977..131073]);
    assert!(text.find(&format!("{}c", "a".repeat(4095))).is_empty());
}
