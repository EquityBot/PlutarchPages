//! Literal search in normalized page text, retaining the original glyph owners.
use super::{PageText, fold_case};
use std::ops::Range;
use unicode_normalization::UnicodeNormalization;

/// Folds script variants a reader can't tell apart, in both the query and the page:
///
/// - Kangxi radicals and the CJK radical supplement forms with a compatibility mapping (U+2E9F,
///   U+2EF3) become their unified ideographs, so `⾒` is found by typing `見`.
/// - Arabic presentation forms become base letters ([`push_presentation_form`]).
/// - Persian yeh and alef maksura become Arabic yeh, and Persian kaf becomes Arabic kaf.
/// - Arabic-Indic and Extended Arabic-Indic digits become ASCII digits.
///
/// Whole-string NFKC is deliberately not used: it would compose `e` + U+0301 so that `e` stops
/// matching, and fold `½` or `™` too. Folding one character at a time also means text
/// that matched before folding still matches after it. One character can become several; each
/// keeps its glyph as owner in [`find`], so a match inside an expansion reports the whole glyph.
fn fold_script(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\u{2E9F}' | '\u{2EF3}' | '\u{2F00}'..='\u{2FD5}' => out.extend(c.nfkc()),
            '\u{FB50}'..='\u{FDFF}' | '\u{FE70}'..='\u{FEFF}' => push_presentation_form(c, &mut out),
            _ => out.push(base_letter(c)),
        }
    }
    out
}

/// A presentation form as its letters, without a ligature's vowel marks and kashida, which a
/// reader searching for the word would not type. A form that is only marks (U+FE70) keeps them
/// and loses its spacing carrier, so it can still be found.
fn push_presentation_form(form: char, out: &mut String) {
    let is_mark = |c: char| matches!(c, '\u{640}' | '\u{64B}'..='\u{65F}' | '\u{670}');
    let has_letter = form.nfkc().any(|c| !c.is_whitespace() && !is_mark(c));
    out.extend(form.nfkc().filter(|&c| if has_letter { !is_mark(c) } else { !c.is_whitespace() && c != '\u{640}' }).map(base_letter));
}

fn base_letter(c: char) -> char {
    match c {
        '\u{6CC}' | '\u{649}' => '\u{64A}',
        '\u{6A9}' => '\u{643}',
        '\u{660}'..='\u{669}' => char::from_digit(u32::from(c) - 0x660, 10).unwrap_or(c),
        '\u{6F0}'..='\u{6F9}' => char::from_digit(u32::from(c) - 0x6F0, 10).unwrap_or(c),
        _ => c,
    }
}

/// `script_variants` is off only for the literal half of redaction search
/// ([`PageText::find_for_redaction`]).
pub(super) fn find(page: &PageText, query: &str, sensitive: bool, whole_words: bool, script_variants: bool, limit: usize) -> Vec<Range<usize>> {
    if limit == 0 {
        return Vec::new();
    }
    let fold = |s: &str| {
        let cased = if sensitive { s.to_owned() } else { fold_case(s) };
        if script_variants { fold_script(&cased) } else { cased }
    };
    let needle: Vec<char> = fold(query).split_whitespace().collect::<Vec<_>>().join(" ").chars().collect();
    if needle.is_empty() {
        return Vec::new();
    }
    let mut chars: Vec<(char, usize)> = Vec::new();
    for (i, glyph) in page.glyphs.iter().enumerate() {
        let line_break = i.checked_sub(1).and_then(|previous| page.line_of.get(previous)).zip(page.line_of.get(i)).is_some_and(|(a, b)| a != b);
        let gap = page.space_before.get(i).copied().unwrap_or(false) || line_break;
        if i > 0 && gap && chars.last().is_some_and(|c| c.0 != ' ') {
            chars.push((' ', i));
        }
        for c in fold(&glyph.text).chars() {
            let c = if c.is_whitespace() { ' ' } else { c };
            if c != ' ' || chars.last().is_none_or(|last| last.0 != ' ') {
                chars.push((c, i));
            }
        }
    }
    if needle.len() > chars.len() {
        return Vec::new();
    }
    // KMP reuses a matched prefix instead of comparing a long query again at every position.
    // The table is bounded by the page's already materialized character count.
    let mut prefix = vec![0; needle.len()];
    let mut matched = 0;
    for (i, &c) in needle.iter().enumerate().skip(1) {
        matched = advance(&needle, &prefix, matched, c);
        if let Some(slot) = prefix.get_mut(i) {
            *slot = matched;
        }
    }
    matched = 0;
    let word = |i: Option<usize>| i.and_then(|i| chars.get(i)).is_some_and(|c| c.0.is_alphanumeric());
    let mut hits = Vec::new();
    for (i, &(c, owner)) in chars.iter().enumerate() {
        matched = advance(&needle, &prefix, matched, c);
        if matched != needle.len() {
            continue;
        }
        let end = i.saturating_add(1);
        let start = end.saturating_sub(needle.len());
        let bounded = !whole_words || (!word(start.checked_sub(1)) && !word(Some(end)));
        if bounded {
            if let Some((_, first)) = chars.get(start)
                && let Some(last) = owner.checked_add(1)
            {
                hits.push(*first..last);
                if hits.len() == limit {
                    break;
                }
            }
            // Only accepted hits consume their full span: literal search is non-overlapping.
            matched = 0;
        } else {
            // A word-boundary rejection must still allow an overlapping later candidate.
            matched = prefix.last().copied().unwrap_or_default();
        }
    }
    hits
}

/// Extend a prefix, falling back through strictly shorter proper prefixes on a mismatch.
fn advance(needle: &[char], prefix: &[usize], mut matched: usize, c: char) -> usize {
    while matched > 0 && needle.get(matched) != Some(&c) {
        matched = prefix.get(matched - 1).copied().unwrap_or_default();
    }
    if needle.get(matched) == Some(&c) { matched.saturating_add(1) } else { 0 }
}
