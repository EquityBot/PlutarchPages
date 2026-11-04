// Copyright 2026 Ryan Duguid
// SPDX-License-Identifier: MIT OR Apache-2.0

//! Which extracted glyphs paint nothing a reader can see, so the tools that hand page text to an
//! agent can say so: text a document hides from its reader is a common route for prompt injection.

use super::device::{Drawn, MAX_CELLS, MAX_REDRAW_WORK, cell, redraw_tolerance};
use super::{GlyphDrawMode, HashMap, Paint};

/// Why a glyph returned with a page's text paints nothing.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HiddenReason {
    /// Text rendering mode 3: neither filled nor stroked.
    Invisible,
    /// Rendering mode 3 inside an `/OCR` marked-content sequence, the layer Scan & OCR ▸
    /// Recognize text writes over a scan. Reported apart, not exempted: any file can write the
    /// marker.
    InvisibleOcrLayer,
    /// Every fill and stroke of it has an effective opacity (its colour's, times that of every
    /// enclosing transparency group) that rounds to 0 in an 8-bit raster.
    Transparent,
}

impl HiddenReason {
    /// The stable machine-readable ID the tools report. Do not rename one; add a variant instead.
    pub fn id(self) -> &'static str {
        match self {
            HiddenReason::Invisible => "invisible",
            HiddenReason::InvisibleOcrLayer => "invisible_ocr_layer",
            HiddenReason::Transparent => "transparent",
        }
    }
}

/// Whether paint at `opacity` leaves nothing in an 8-bit raster: it rounds to an alpha of 0.
/// A NaN opacity paints, as an unreadable one is not evidence of hiding.
pub fn paints_nothing(opacity: f64) -> bool {
    opacity < 0.5 / 255.0
}

/// The class of a glyph drawn several times (a fill and a stroke, or the glyphs one
/// `/ActualText` replaces): hidden only when no drawing paints. A reason other than the OCR
/// layer wins, so a marker cannot soften what another drawing shows.
pub(super) fn combine(a: Option<HiddenReason>, b: Option<HiddenReason>) -> Option<HiddenReason> {
    match (a, b) {
        (Some(HiddenReason::InvisibleOcrLayer), b @ Some(_)) => b,
        (a @ Some(_), Some(_)) => a,
        _ => None,
    }
}

/// What makes the glyphs a page draws paint nothing, followed while it is interpreted.
#[derive(Default)]
pub(super) struct Visibility {
    /// Whether to classify glyphs at all: plain extraction skips the colour conversion.
    pub(super) on: bool,
    /// The marked-content depth of the outermost open `/OCR` sequence.
    ocr: Option<usize>,
    /// The opacity of each open transparency group, outermost first.
    groups: Vec<f32>,
}

impl Visibility {
    pub(super) fn begin_marked_content(&mut self, depth: usize, tag: &[u8]) {
        if self.ocr.is_none() && tag == b"OCR" {
            self.ocr = Some(depth);
        }
    }

    pub(super) fn end_marked_content(&mut self, depth: usize) {
        if self.ocr == Some(depth) {
            self.ocr = None;
        }
    }

    /// hayro pushes and pops groups in pairs, also for an image or a knockout pass, so the stack
    /// is never deeper than the content's nesting.
    pub(super) fn push_group(&mut self, opacity: f32) {
        self.groups.push(opacity);
    }

    pub(super) fn pop_group(&mut self) {
        self.groups.pop();
    }

    /// Why one drawing of a glyph in `mode` with `paint` paints nothing, if it does not paint.
    /// Clipping text (`7 Tr`) is not classed: what it shows is what is painted through it.
    pub(super) fn class(&self, paint: &Paint<'_>, mode: &GlyphDrawMode) -> Option<HiddenReason> {
        if !self.on {
            return None;
        }
        match mode {
            GlyphDrawMode::Invisible if self.ocr.is_some() => Some(HiddenReason::InvisibleOcrLayer),
            GlyphDrawMode::Invisible => Some(HiddenReason::Invisible),
            GlyphDrawMode::Clip => None,
            GlyphDrawMode::Fill | GlyphDrawMode::Stroke(_) => {
                // shortcut: a pattern's opacity has no getter in hayro, so pattern paint counts as
                // painting; reading it needs an accessor patched into the vendored `Pattern`.
                let Paint::Color(colour) = paint else { return None };
                // The renderer's own alpha: 0 for a Separation /None, opaque for a colour it
                // cannot convert (drawn in a fallback colour).
                let alpha = self.groups.iter().fold(colour.to_rgba().components()[3], |alpha, group| alpha * group);
                paints_nothing(f64::from(alpha)).then_some(HiddenReason::Transparent)
            }
        }
    }
}

/// A hidden glyph with a painting glyph of the same text at its place shows after all: `2 Tr`
/// draws each glyph once to fill and once to stroke, and a file may draw a hidden copy before a
/// visible one. Layout keeps the first drawing of a glyph drawn twice, so the classes are joined
/// before it, as `TextDevice::finish` matches redraws. Past the work budget the rest stay
/// hidden.
pub(super) fn cover_redrawn(glyphs: &mut [Drawn]) {
    let mut grid: HashMap<(i64, i64), Vec<usize>> = HashMap::new();
    for (i, g) in glyphs.iter().enumerate() {
        if g.3.is_none() {
            grid.entry((cell(g.2.rect[0]), cell(g.2.rect[1]))).or_default().push(i);
        }
    }
    if grid.is_empty() {
        return;
    }
    let mut shown = Vec::new();
    let mut budget = MAX_REDRAW_WORK;
    'glyphs: for (i, g) in glyphs.iter().enumerate() {
        if g.3.is_none() {
            continue;
        }
        let rect = &g.2.rect;
        let tol = redraw_tolerance(rect);
        let (x0, x1, y0, y1) = (cell(rect[0] - tol), cell(rect[0] + tol), cell(rect[1] - tol), cell(rect[1] + tol));
        if x1.saturating_sub(x0).saturating_add(1).saturating_mul(y1.saturating_sub(y0).saturating_add(1)) > MAX_CELLS {
            continue;
        }
        for key in (x0..=x1).flat_map(|x| (y0..=y1).map(move |y| (x, y))) {
            for &j in grid.get(&key).map_or(&[][..], Vec::as_slice) {
                let Some(rest) = budget.checked_sub(1) else { break 'glyphs };
                budget = rest;
                let Some(other) = glyphs.get(j) else { continue };
                let o = &other.2.rect;
                if other.2.text == g.2.text && (o[0] - rect[0]).abs() < tol && (o[1] - rect[1]).abs() < tol {
                    shown.push(i);
                    continue 'glyphs;
                }
            }
            let Some(rest) = budget.checked_sub(1) else { break 'glyphs };
            budget = rest;
        }
    }
    for i in shown {
        if let Some(g) = glyphs.get_mut(i) {
            g.3 = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::HiddenReason::{self, Invisible, InvisibleOcrLayer, Transparent};
    use crate::text::{InterpreterSettings, Pdf, extract_page, extract_page_hidden};

    /// A 400 × 400 page drawing `content`, with Helvetica as /F1, /GS0 (fill opacity 0), /GS3
    /// (fill and stroke opacity 0), a grey 1 × 1 image /Im0 and a transparency group /Fm0 that
    /// shows "Inside the group".
    fn page(content: &str) -> Pdf {
        let group = "BT /F1 20 Tf 1 0 0 1 50 200 Tm (Inside the group) Tj ET";
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_string(),
            "<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_string(),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_string(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 400 400] /Contents 5 0 R /Resources << /Font << /F1 3 0 R >> \
             /ExtGState << /GS0 << /ca 0 >> /GS3 << /ca 0 /CA 0 >> >> /XObject << /Im0 6 0 R /Fm0 7 0 R >> >> >>"
                .to_string(),
            format!("<< /Length {} >>\nstream\n{content}\nendstream", content.len()),
            "<< /Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 \
             /Filter /ASCIIHexDecode /Length 3 >>\nstream\n80>\nendstream"
                .to_string(),
            format!(
                "<< /Type /XObject /Subtype /Form /BBox [0 0 400 400] /Group << /S /Transparency >> \
                 /Resources << /Font << /F1 3 0 R >> >> /Length {} >>\nstream\n{group}\nendstream",
                group.len()
            ),
        ];
        let mut bytes = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (i, object) in objects.iter().enumerate() {
            offsets.push(bytes.len());
            bytes.extend_from_slice(format!("{} 0 obj\n{object}\nendobj\n", i + 1).as_bytes());
        }
        let xref = bytes.len();
        bytes.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
        for offset in offsets {
            bytes.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        bytes.extend_from_slice(format!("trailer << /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n", objects.len() + 1).as_bytes());
        Pdf::new(std::sync::Arc::new(bytes)).unwrap()
    }

    /// Each returned glyph's text with its class. The classes never change the text.
    fn classified(content: &str) -> Vec<(String, Option<HiddenReason>)> {
        let pdf = page(content);
        let (text, hidden) = extract_page_hidden(&pdf, 0, &InterpreterSettings::default()).unwrap();
        assert_eq!(hidden.len(), text.glyphs.len(), "one class per returned glyph");
        assert_eq!(Some(&text), extract_page(&pdf, 0, &InterpreterSettings::default()).as_ref(), "the same text as plain extraction");
        text.glyphs.iter().map(|g| g.text.clone()).zip(hidden).collect()
    }

    /// The text of the glyphs classed `reason`, in order, without spaces.
    fn flagged(glyphs: &[(String, Option<HiddenReason>)], reason: HiddenReason) -> String {
        glyphs.iter().filter(|(_, r)| *r == Some(reason)).flat_map(|(t, _)| t.chars()).filter(|c| !c.is_whitespace()).collect()
    }

    /// `text` at height `y` in rendering mode `mode`, which stays set after the text object.
    fn line(y: u32, mode: u8, text: &str) -> String {
        format!("BT /F1 20 Tf {mode} Tr 1 0 0 1 50 {y} Tm ({text}) Tj ET\n")
    }

    #[test]
    fn ordinary_text_is_not_hidden() {
        let glyphs = classified(&line(300, 0, "Ordinary text"));
        assert!(glyphs.iter().all(|(_, r)| r.is_none()), "{glyphs:?}");
    }

    #[test]
    fn rendering_mode_3_and_zero_fill_opacity_are_hidden() {
        let content = format!("{}{}q /GS0 gs {}Q", line(300, 0, "Shown"), line(250, 3, "Unseen"), line(200, 0, "Clear"));
        let glyphs = classified(&content);
        assert_eq!(flagged(&glyphs, Invisible), "Unseen");
        assert_eq!(flagged(&glyphs, Transparent), "Clear");
        assert_eq!(flagged(&glyphs, InvisibleOcrLayer), "");
    }

    #[test]
    fn a_stroke_that_shows_outlines_a_transparent_fill() {
        let content = format!("q /GS0 gs 0 0 1 RG 1 w {}Q q /GS3 gs 1 w {}Q", line(300, 2, "Outlined"), line(200, 2, "Neither"));
        let glyphs = classified(&content);
        assert_eq!(flagged(&glyphs, Transparent), "Neither", "the fill and stroke of 2 Tr are one glyph: hidden only when both paint nothing");
    }

    #[test]
    fn a_hidden_copy_drawn_again_visibly_shows() {
        for content in
            [format!("{}{}", line(300, 3, "Secret"), line(300, 0, "Secret")), format!("{}{}", line(300, 0, "Secret"), line(300, 3, "Secret"))]
        {
            let glyphs = classified(&content);
            assert_eq!(glyphs.iter().map(|(t, _)| t.as_str()).collect::<String>(), "Secret", "layout keeps one drawing");
            assert!(glyphs.iter().all(|(_, r)| r.is_none()), "{content}: {glyphs:?}");
        }
    }

    #[test]
    fn classes_stay_with_their_glyphs_through_redraws_and_actual_text() {
        let content = [
            line(350, 0, "Dup"),
            // Drawn again at the same place: layout drops this copy.
            line(350, 0, "Dup"),
            format!("/Span <</ActualText (Word)>> BDC {}EMC\n", line(300, 0, "xyz")),
            line(250, 3, "Hidden"),
            format!("/Span <</ActualText (Gone)>> BDC {}EMC\n", line(200, 3, "abc")),
            line(150, 0, "End"),
        ]
        .concat();
        let glyphs = classified(&content);
        let text: Vec<&str> = glyphs.iter().map(|(t, _)| t.as_str()).collect();
        assert_eq!(text, ["D", "u", "p", "Word", "H", "i", "d", "d", "e", "n", "Gone", "E", "n", "d"]);
        assert_eq!(flagged(&glyphs, Invisible), "HiddenGone", "{glyphs:?}");
    }

    #[test]
    fn the_ocr_layer_is_reported_with_its_own_reason() {
        let content = format!(
            "/OCR BMC q {}/Span BMC {}EMC {}Q EMC {}",
            line(300, 3, "Scanned"),
            line(250, 3, "Nested"),
            line(200, 3, "Words"),
            line(150, 3, "Outside")
        );
        let glyphs = classified(&content);
        assert_eq!(flagged(&glyphs, InvisibleOcrLayer), "ScannedNestedWords", "an inner sequence does not end the layer");
        assert_eq!(flagged(&glyphs, Invisible), "Outside");
    }

    #[test]
    fn invisible_text_over_a_picture_is_not_exempt() {
        let glyphs = classified(&format!("q 400 0 0 400 0 0 cm /Im0 Do Q {}", line(200, 3, "Over")));
        assert_eq!(flagged(&glyphs, Invisible), "Over");
    }

    #[test]
    fn a_transparency_group_hides_its_text_until_it_ends() {
        let glyphs = classified(&format!("q /GS0 gs /Fm0 Do Q {}", line(300, 0, "After")));
        // "After" reads first and is not flagged: the group's opacity ended with it.
        assert_eq!(flagged(&glyphs, Transparent), "Insidethegroup", "the group's opacity is 0 and its text is opaque inside it");
    }
}
