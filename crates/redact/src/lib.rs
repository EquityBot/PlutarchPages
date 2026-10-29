//! Redaction (architecture §11.2, execution plan M8.5–M8.7). Layer L4.
//!
//! Content is marked for redaction with Redact annotations (§12.5.6.23); `pdfcraft-annot`
//! creates them. [`apply`] then removes everything under the marks, for good:
//! - text glyphs (the rest of each line keeps its position), inline images, and paths that the
//!   marks cover; images and vectors partly under a mark lose the covered part;
//! - content inside form XObjects (rewritten as new objects, so pages sharing them keep theirs);
//! - comments, links and form fields whose rectangle overlaps a mark;
//! - the marks themselves, replaced by boxes in their fill colour (with their overlay text)
//!   drawn into the page.
//!
//! A verification pass then re-reads every redacted page; if any glyph or inline image is still
//! under a region the whole operation fails (callers keep the previous document: fail-closed).
//! It fails the same way when content under the marks is also used where the marks don't reach
//! (another page, or another draw outside the marks), since its original would stay in the file.

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

pub mod codes;
mod image;
mod interp;
pub mod patterns;
pub mod sanitize;
mod tags;
#[cfg(test)]
mod tests;

use interp::{Mode, Scope, process};

pub type Rgb = [f64; 3];

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RedactError {
    #[error("there are no redaction marks to apply")]
    NothingToApply,
    #[error("page {0} has content that can't be read, so it can't be redacted safely")]
    Unreadable(usize),
    #[error("redaction could not be verified: {0} item(s) were still found under the marks, so nothing was changed")]
    Residue(usize),
    /// Content under the marks is shared with places the marks don't cover (other pages, or
    /// another draw outside the marks): its unredacted original would stay in the file.
    #[error(
        "content under the marks is also used {}, outside the marks, so its unredacted original would stay in the file; mark it there too. Nothing was changed",
        where_used(.0)
    )]
    StillUsed(Vec<usize>),
    /// A form under the marks is drawn so that its marks can't be carried to its other draws.
    #[error(
        "a form under the marks is drawn {} in a way its marks can't be carried to its other appearances, so it can't be redacted safely. Nothing was changed",
        where_used(.0)
    )]
    SharedForm(Vec<usize>),
    #[error(transparent)]
    Edit(#[from] pdfcraft_edit::EditError),
    #[error(transparent)]
    Form(#[from] pdfcraft_forms::FormError),
    #[error(transparent)]
    Cos(#[from] pdfcraft_cos::CosError),
}

/// A redaction mark on a page.
#[derive(Clone, Debug, PartialEq)]
pub struct Mark {
    /// 0-based page index.
    pub page: usize,
    pub obj: ObjRef,
    /// The marked areas in user space (one per quadrilateral).
    pub rects: Vec<[f64; 4]>,
    /// The box colour once applied (`/IC`; none = no box).
    pub fill: Option<Rgb>,
    pub overlay: String,
    /// How the overlay text is drawn (`/DA`, `/Q`, `/Repeat`).
    pub look: pdfcraft_annot::OverlayLook,
}

/// The overlay look of a Redact annotation: font, size and colour from `/DA`, alignment from
/// `/Q`, repetition from `/Repeat`.
fn overlay_look(doc: &Document, d: &Dict) -> pdfcraft_annot::OverlayLook {
    let mut look = pdfcraft_annot::OverlayLook::default();
    if let Some(da) = d.get(b"DA").and_then(|o| doc.resolve(o).as_string().map(|s| String::from_utf8_lossy(&s.bytes).into_owned())) {
        let t: Vec<&str> = da.split_whitespace().collect();
        for (i, w) in t.iter().enumerate() {
            match *w {
                "rg" if i >= 3 => {
                    if let (Ok(r), Ok(g), Ok(b)) = (t[i - 3].parse::<f64>(), t[i - 2].parse::<f64>(), t[i - 1].parse::<f64>()) {
                        look.color = [r, g, b];
                    }
                }
                "g" if i >= 1 => {
                    if let Ok(g) = t[i - 1].parse::<f64>() {
                        look.color = [g; 3];
                    }
                }
                "Tf" if i >= 2 => {
                    look.font = pdfcraft_annot::OverlayFont::from_resource(t[i - 2].trim_start_matches('/'));
                    look.size = t[i - 1].parse::<f64>().unwrap_or(0.0).max(0.0);
                }
                _ => {}
            }
        }
    }
    look.align = d.int(b"Q").unwrap_or(1).clamp(0, 2) as u8;
    look.repeat = matches!(d.get(b"Repeat"), Some(Object::Bool(true)));
    look
}

/// What [`apply`] removed.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Report {
    pub marks: usize,
    pub pages: usize,
    pub glyphs: usize,
    pub images_removed: usize,
    pub images_cleared: usize,
    pub paths_removed: usize,
    pub paths_clipped: usize,
    pub forms_rewritten: usize,
    pub forms_removed: usize,
    pub annotations: usize,
    pub fields: usize,
    /// Structure elements that lost alternate/actual text or emptied marked content.
    pub tags: usize,
    /// Draws of an image or form marked in more than one place, redacted with the marks from
    /// all of them (so a draw can lose more than its own marks cover).
    pub shared_redacted: usize,
}

fn rect_of(doc: &Document, o: Option<&Object>) -> Option<[f64; 4]> {
    let v: Vec<f64> = doc.resolve(o?).as_array()?.iter().filter_map(|x| doc.resolve(x).as_f64()).collect();
    (v.len() == 4 && v.iter().all(|x| x.is_finite())).then(|| [v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

fn color(doc: &Document, d: &Dict, key: &[u8]) -> Option<Rgb> {
    let v: Vec<f64> = doc.resolve(d.get(key)?).as_array()?.iter().filter_map(Object::as_f64).collect();
    match v.len() {
        1 => Some([v[0]; 3]),
        3 => Some([v[0], v[1], v[2]]),
        4 => Some([(1.0 - v[0]) * (1.0 - v[3]), (1.0 - v[1]) * (1.0 - v[3]), (1.0 - v[2]) * (1.0 - v[3])]),
        _ => None,
    }
}

pub(crate) fn annots_of(doc: &Document, page: &Dict) -> Vec<Object> {
    page.get(b"Annots").map(|a| doc.resolve(a)).and_then(|a| a.as_array().cloned()).unwrap_or_default()
}

/// Every redaction mark in the document, page by page.
pub fn marks(doc: &Document) -> Vec<Mark> {
    let mut out = Vec::new();
    for (pi, p) in pdfcraft_model::pages(doc).iter().enumerate() {
        for a in annots_of(doc, &p.dict) {
            let Some(r) = a.as_ref() else { continue };
            let obj = doc.get(r);
            let Some(d) = obj.as_dict() else { continue };
            if d.name(b"Subtype") != Some(b"Redact") {
                continue;
            }
            let quads: Vec<f64> = d
                .get(b"QuadPoints")
                .and_then(|q| doc.resolve(q).as_array().map(|a| a.iter().filter_map(Object::as_f64).collect()))
                .unwrap_or_default();
            let mut rects: Vec<[f64; 4]> = quads
                .as_chunks::<8>()
                .0
                .iter()
                .map(|q| {
                    let xs = [q[0], q[2], q[4], q[6]];
                    let ys = [q[1], q[3], q[5], q[7]];
                    [
                        xs.iter().copied().fold(f64::MAX, f64::min),
                        ys.iter().copied().fold(f64::MAX, f64::min),
                        xs.iter().copied().fold(f64::MIN, f64::max),
                        ys.iter().copied().fold(f64::MIN, f64::max),
                    ]
                })
                .filter(|r| r.iter().all(|v| v.is_finite()))
                .collect();
            if rects.is_empty()
                && let Some(r) = rect_of(doc, d.get(b"Rect"))
            {
                rects.push(r);
            }
            if rects.is_empty() {
                continue;
            }
            let overlay = d.get(b"OverlayText").and_then(|t| doc.resolve(t).as_string().map(|s| s.to_text())).unwrap_or_default();
            out.push(Mark { page: pi, obj: r, rects, fill: color(doc, d, b"IC"), overlay, look: overlay_look(doc, d) });
        }
    }
    out
}

/// A page's content streams: (the references or inline objects as listed, their decoded data).
pub(crate) fn page_streams(doc: &Document, page: &Dict, index: usize) -> Result<(Vec<Object>, Vec<Vec<u8>>), RedactError> {
    let list: Vec<Object> = match page.get(b"Contents") {
        None => Vec::new(),
        Some(c) => match &*doc.resolve(c) {
            Object::Array(a) => a.clone(),
            _ => vec![c.clone()],
        },
    };
    let mut data = Vec::new();
    for o in &list {
        match &*doc.resolve(o) {
            Object::Stream(s) => data.push(s.decoded().map_err(|_| RedactError::Unreadable(index + 1))?),
            Object::Null => data.push(Vec::new()),
            _ => return Err(RedactError::Unreadable(index + 1)),
        }
    }
    Ok((list, data))
}

/// The boxes and overlay text drawn for the applied marks.
fn overlay_content(marks: &[&Mark]) -> Vec<u8> {
    let n = |v: f64| {
        let s = format!("{:.3}", v);
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    };
    let mut c: Vec<u8> = Vec::new();
    for m in marks {
        let Some(fill) = m.fill else { continue };
        c.extend(format!("q {} {} {} rg\n", n(fill[0]), n(fill[1]), n(fill[2])).bytes());
        for r in &m.rects {
            c.extend(format!("{} {} {} {} re f\n", n(r[0]), n(r[1]), n(r[2] - r[0]), n(r[3] - r[1])).bytes());
        }
        c.extend_from_slice(b"Q\n");
        if m.overlay.is_empty() {
            continue;
        }
        // The overlay text: its font, colour and alignment; auto-sized to fit unless a size is
        // set; repeated to fill the area when asked.
        let look = &m.look;
        let (res, width): (&str, fn(&str, f64) -> f64) = match look.font {
            pdfcraft_annot::OverlayFont::Helvetica => ("PCHelv", pdfcraft_fonts::helvetica_width),
            // Approximations of the standard metrics (no font program is bundled).
            pdfcraft_annot::OverlayFont::Times => ("PCTimes", |s, size| pdfcraft_fonts::helvetica_width(s, size) * 0.9),
            pdfcraft_annot::OverlayFont::Courier => ("PCCour", |s, size| s.chars().count() as f64 * size * 0.6),
        };
        let [cr, cg, cb] = look.color.map(|v| v.clamp(0.0, 1.0));
        for r in &m.rects {
            let (w, h) = (r[2] - r[0], r[3] - r[1]);
            let mut size = if look.size > 0.0 { look.size } else { (h * 0.7).min(12.0) };
            let tw = width(&m.overlay, size);
            if look.size <= 0.0 && tw > w - 2.0 && tw > 0.0 {
                size *= (w - 2.0).max(0.0) / tw;
            }
            if size < 2.0 {
                continue;
            }
            // One line, or as many repeated lines as fit (each line the text repeated across).
            let lines: Vec<String> = if look.repeat {
                let unit = width(&format!("{} ", m.overlay), size).max(0.01);
                let per_line = ((w - 2.0) / unit).floor().max(1.0) as usize;
                let count = ((h / (size * 1.2)).floor() as usize).max(1);
                vec![vec![m.overlay.as_str(); per_line].join(" "); count]
            } else {
                vec![m.overlay.clone()]
            };
            let block = lines.len() as f64 * size * 1.2;
            let mut y = r[1] + (h + block) / 2.0 - size * 0.95;
            c.extend(
                format!("q {} {} {} {} re W n BT {} {} {} rg /{res} {} Tf ", n(r[0]), n(r[1]), n(w), n(h), n(cr), n(cg), n(cb), n(size)).bytes(),
            );
            for line in &lines {
                let lw = width(line, size);
                let x = match look.align {
                    0 => r[0] + 1.0,
                    2 => r[2] - 1.0 - lw,
                    _ => r[0] + (w - lw) / 2.0,
                };
                c.extend(format!("1 0 0 1 {} {} Tm ", n(x), n(y)).bytes());
                c.extend_from_slice(&pdfcraft_fonts::literal(&pdfcraft_fonts::win_ansi(line)));
                c.extend_from_slice(b" Tj ");
                y -= size * 1.2;
            }
            c.extend_from_slice(b"ET Q\n");
        }
    }
    c
}

/// "on page 2", "on pages 2, 5", or "elsewhere in the document" (`pages` 0-based).
fn where_used(pages: &[usize]) -> String {
    match pages {
        [] => "elsewhere in the document".into(),
        [p] => format!("on page {}", p + 1),
        _ => format!("on pages {}", pages.iter().map(|p| (p + 1).to_string()).collect::<Vec<_>>().join(", ")),
    }
}

/// Push the references in `o` (and in the arrays and dictionaries it holds) onto `out`. Iterative,
/// so deeply nested objects can't exhaust the stack.
fn refs_in(o: &Object, out: &mut Vec<ObjRef>) {
    let mut stack = vec![o];
    while let Some(o) = stack.pop() {
        match o {
            Object::Ref(r) => out.push(*r),
            Object::Array(a) => stack.extend(a.iter()),
            Object::Dict(d) => stack.extend(d.iter().map(|(_, v)| v)),
            Object::Stream(s) => stack.extend(s.dict.iter().map(|(_, v)| v)),
            _ => {}
        }
    }
}

/// The objects reachable from `roots` by following references.
fn reachable(doc: &Document, roots: &[Object]) -> HashSet<ObjRef> {
    let mut queue = Vec::new();
    roots.iter().for_each(|o| refs_in(o, &mut queue));
    let mut seen = HashSet::new();
    while let Some(r) = queue.pop() {
        if seen.insert(r) {
            refs_in(&doc.get(r), &mut queue);
        }
    }
    seen
}

/// The most content decoded to find the XObject names a page's content draws.
const DRAWN_BUDGET: usize = 64 << 20;

/// Could this stream be drawn as content (forms, patterns, Type 3 glyphs, appearances)? A form
/// always can; images, font programs, colour profiles, metadata and embedded files can't.
fn content_like(d: &Dict) -> bool {
    match d.name(b"Subtype") {
        Some(b"Form") => true,
        Some(b"Image" | b"Type1C" | b"CIDFontType0C" | b"OpenType" | b"XML") => false,
        _ => {
            !(matches!(d.name(b"Type"), Some(b"Metadata" | b"EmbeddedFile" | b"XRef" | b"ObjStm"))
                || [&b"Length1"[..], b"Length2", b"Length3", b"N"].iter().any(|k| d.contains(k)))
        }
    }
}

/// The names `Do` operators use in `ops`.
fn do_names(ops: &[pdfcraft_content::Op]) -> impl Iterator<Item = Vec<u8>> + '_ {
    ops.iter().filter(|op| op.is("Do")).filter_map(|op| op.name(0).map(<[u8]>::to_vec))
}

/// Every name a `Do` operator uses in page `index`'s contents (its streams read as one, as
/// operators may be split between them) or in any content reachable from `roots` (its resources
/// and annotation appearances, including forms that draw from the resources they inherit).
/// `None`, so that every name counts as drawn, when some of that content can't be read or is
/// too large to read.
fn drawn_names(doc: &Document, page: &Dict, index: usize, roots: &[Object]) -> Option<HashSet<Vec<u8>>> {
    let (_, data) = page_streams(doc, page, index).ok()?;
    let mut names: HashSet<Vec<u8>> = do_names(&pdfcraft_content::Pieces::join(&data).parse().ops).collect();
    let mut budget = DRAWN_BUDGET.checked_sub(data.iter().map(Vec::len).sum())?;
    let mut queue = Vec::new();
    roots.iter().for_each(|o| refs_in(o, &mut queue));
    let mut seen = HashSet::new();
    while let Some(r) = queue.pop() {
        if !seen.insert(r) {
            continue;
        }
        let obj = doc.get(r);
        refs_in(&obj, &mut queue);
        let Object::Stream(s) = &*obj else { continue };
        if !content_like(&s.dict) {
            continue;
        }
        let data = s.decoded().ok()?;
        budget = budget.checked_sub(data.len())?;
        names.extend(do_names(&pdfcraft_content::parse(&data).ops));
    }
    Some(names)
}

/// `res` without the image and form XObjects whose original redaction retired (none of them
/// is drawn where this is used; see [`check_retired`]).
fn without_retired(doc: &Document, res: &Dict, retired: &HashSet<ObjRef>) -> Option<Dict> {
    let xo = res.get(b"XObject").and_then(|x| doc.resolve(x).as_dict().cloned())?;
    if !xo.iter().any(|(_, v)| v.as_ref().is_some_and(|r| retired.contains(&r))) {
        return None;
    }
    let kept: Dict = xo.iter().filter(|(_, v)| v.as_ref().is_none_or(|r| !retired.contains(&r))).map(|(k, v)| (k.clone(), v.clone())).collect();
    let mut res = res.clone();
    res.set(b"XObject".to_vec(), Object::Dict(kept));
    Some(res)
}

/// Fail closed when an original that redaction replaced or removed (an image, a form, a content
/// stream) could still be saved with the file: a page draws it outside the marks (those pages
/// are named), or something else still refers to it. Resource entries for it that no content
/// names, such as those inherited from the page tree, are dropped first: each page and page-tree
/// node gets its own copy of its resources without them, so a resource dictionary shared with
/// anything else is left as it is (and then fails the final check if it still reaches them).
fn check_retired(doc: &mut Document, retired: &HashSet<ObjRef>) -> Result<(), RedactError> {
    let all = pdfcraft_model::pages(doc);
    let mut using = Vec::new();
    for (i, p) in all.iter().enumerate() {
        // What the page draws: its contents, the XObjects named by any content it can reach
        // (forms drawing from inherited resources included), its other resources, and
        // annotation appearances.
        let res = p.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned());
        let appearances: Vec<Object> = annots_of(doc, &p.dict).iter().filter_map(|a| doc.resolve(a).as_dict()?.get(b"AP").cloned()).collect();
        let drawn_from: Vec<Object> = appearances.iter().cloned().chain(res.clone().map(Object::Dict)).collect();
        let drawn = drawn_names(doc, &p.dict, i, &drawn_from);
        let mut roots: Vec<Object> = p.dict.get(b"Contents").cloned().into_iter().chain(appearances).collect();
        if let Some(mut res) = res {
            if let (Some(drawn), Some(xo)) = (&drawn, res.get(b"XObject").and_then(|x| doc.resolve(x).as_dict().cloned())) {
                let used: Dict = xo.iter().filter(|(k, _)| drawn.contains(k.as_slice())).map(|(k, v)| (k.clone(), v.clone())).collect();
                res.set(b"XObject".to_vec(), Object::Dict(used));
            }
            roots.push(Object::Dict(res));
        }
        if !reachable(doc, &roots).is_disjoint(retired) {
            using.push(i);
        }
    }
    if !using.is_empty() {
        return Err(RedactError::StillUsed(using));
    }
    // No content names them: give every page and page-tree node its own resources without them.
    let mut holders: Vec<ObjRef> = all.iter().map(|p| p.obj).collect();
    let mut node = doc.root().and_then(|r| doc.get(r).as_dict()?.get(b"Pages")?.as_ref());
    let mut nodes = Vec::new();
    while let Some(n) = node.take().or_else(|| nodes.pop()) {
        if holders.contains(&n) {
            continue;
        }
        holders.push(n);
        if let Some(kids) = doc.get(n).as_dict().and_then(|d| d.get(b"Kids").map(|k| doc.resolve(k).as_array().cloned().unwrap_or_default())) {
            nodes.extend(kids.iter().filter_map(Object::as_ref));
        }
    }
    for h in holders {
        let holder = doc.get(h);
        let Some(res) = holder.as_dict().and_then(|d| d.get(b"Resources")).and_then(|r| doc.resolve(r).as_dict().cloned()) else { continue };
        if let Some(pruned) = without_retired(doc, &res, retired) {
            doc.update_dict(h, |d| d.set(b"Resources".to_vec(), Object::Dict(pruned)))?;
        }
    }
    let roots: Vec<Object> = [&b"Root"[..], b"Info", b"Encrypt"].iter().filter_map(|k| doc.trailer().get(k).cloned()).collect();
    if reachable(doc, &roots).is_disjoint(retired) { Ok(()) } else { Err(RedactError::StillUsed(Vec::new())) }
}

/// Apply every redaction mark (or only those on `pages`, 0-based). Irreversible for the saved
/// file; callers keep the previous document for undo. Refused (with the document left for the
/// caller to discard) when content under the marks is also used where the marks don't reach.
pub fn apply(doc: &mut Document, pages: Option<&[usize]>) -> Result<Report, RedactError> {
    let all_marks = marks(doc);
    let chosen: Vec<&Mark> = all_marks.iter().filter(|m| pages.is_none_or(|p| p.contains(&m.page))).collect();
    if chosen.is_empty() {
        return Err(RedactError::NothingToApply);
    }
    let mut report = Report { marks: chosen.len(), ..Report::default() };
    let mut by_page: Vec<usize> = chosen.iter().map(|m| m.page).collect();
    by_page.sort_unstable();
    by_page.dedup();
    report.pages = by_page.len();
    let mut doomed_fields: Vec<String> = Vec::new();
    let widget_owner: Vec<(ObjRef, String)> =
        pdfcraft_forms::fields(doc).into_iter().flat_map(|f| f.widgets.iter().map(|w| (w.obj, f.name.clone())).collect::<Vec<_>>()).collect();
    let mut retired: HashSet<ObjRef> = HashSet::new();
    // Annotations dropped from page /Annots (under a mark, replies to those, their pop-ups).
    let mut gone_annots: HashSet<ObjRef> = HashSet::new();

    // 0. Where each form XObject is drawn over the marks, on every page, so each such draw is
    // redacted with the marks of all of them (changes nothing).
    let mut shared = interp::Shared { collect: true, ..Default::default() };
    for &pi in &by_page {
        let page = pdfcraft_model::pages(doc).swap_remove(pi);
        let rects: Vec<[f64; 4]> = chosen.iter().filter(|m| m.page == pi).flat_map(|m| m.rects.iter().copied()).collect();
        let (_, data) = page_streams(doc, &page.dict, pi)?;
        let resources = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
        let mut scratch = Report::default();
        let mut scope = Scope::new(&rects, Mode::Verify, &mut scratch);
        scope.page = pi;
        scope.shared = std::mem::take(&mut shared);
        process(doc, &mut scope, &data, &resources, pdfcraft_content::Matrix::IDENTITY);
        shared = std::mem::take(&mut scope.shared);
    }
    shared.collect = false;

    for &pi in &by_page {
        let page = pdfcraft_model::pages(doc).swap_remove(pi);
        let page_marks: Vec<&Mark> = chosen.iter().copied().filter(|m| m.page == pi).collect();
        let rects: Vec<[f64; 4]> = page_marks.iter().flat_map(|m| m.rects.iter().copied()).collect();

        // 1. Content.
        let (list, data) = page_streams(doc, &page.dict, pi)?;
        let resources = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
        let out = {
            let mut scope = Scope::new(&rects, Mode::Apply, &mut report);
            scope.page = pi;
            scope.shared = std::mem::take(&mut shared);
            let out = process(doc, &mut scope, &data, &resources, pdfcraft_content::Matrix::IDENTITY);
            shared = std::mem::take(&mut scope.shared);
            out
        };
        retired.extend(&out.retired);
        let mut new_list = list.clone();
        let mut changed = false;
        for (i, new) in out.streams.into_iter().enumerate() {
            let Some(bytes) = new else { continue };
            // The original stream holds the redacted content; another page may still use it.
            if let Some(r) = list.get(i).and_then(Object::as_ref) {
                retired.insert(r);
            }
            let mut dict = match &*doc.resolve(&list[i]) {
                Object::Stream(s) => s.dict.clone(),
                _ => Dict::new(),
            };
            dict.remove(b"Length");
            // Edit ▸ Add content keeps an added item's source text in `/PCAdded`; once its glyphs
            // are redacted, keeping it would leave the redacted text in the file. The item stays
            // as plain drawn content but is no longer editable.
            dict.remove(b"PCAdded");
            // A new object: the original may be shared with other pages.
            new_list[i] = Object::Ref(doc.add(Object::Stream(Stream::flate(dict, &bytes))));
            changed = true;
        }
        if changed || !out.xobjects.is_empty() || !out.remove_xobjects.is_empty() {
            let mut res = resources;
            if !out.xobjects.is_empty() || !out.remove_xobjects.is_empty() {
                let mut xo = res.get(b"XObject").and_then(|x| doc.resolve(x).as_dict().cloned()).unwrap_or_default();
                // Drop originals of redacted images first, then add the cleared copies (#389).
                for n in &out.remove_xobjects {
                    xo.remove(n);
                }
                for (n, r) in &out.xobjects {
                    xo.set(n.clone(), Object::Ref(*r));
                }
                res.set(b"XObject".to_vec(), Object::Dict(xo));
            }
            doc.update_dict(page.obj, |d| {
                d.set(b"Contents".to_vec(), Object::Array(new_list));
                d.set(b"Resources".to_vec(), Object::Dict(res));
            })?;
            // Tags must not keep what the page no longer shows.
            let after = doc.get(page.obj).as_dict().cloned().unwrap_or_default();
            let (_, new_data) = page_streams(doc, &after, pi)?;
            let (changed_ids, empty_ids) = tags::touched(&data.join(&b'\n'), &new_data.join(&b'\n'));
            report.tags += tags::clean(doc, page.obj, &changed_ids, &empty_ids)?;
        }

        // 2. Annotations: the marks, and whatever lies under them (with their pop-ups).
        let annots = annots_of(doc, &doc.get(page.obj).as_dict().cloned().unwrap_or_default());
        let mut removed: HashSet<ObjRef> = HashSet::new();
        let under = |d: &Dict| rect_of(doc, d.get(b"Rect")).is_some_and(|b| rects.iter().any(|x| pdfcraft_content::overlaps(*x, b, 0.0)));
        for a in &annots {
            let Some(r) = a.as_ref() else { continue };
            let obj = doc.get(r);
            let Some(d) = obj.as_dict() else { continue };
            let subtype = d.name(b"Subtype").unwrap_or(b"");
            if subtype == b"Redact" {
                if page_marks.iter().any(|m| m.obj == r) {
                    removed.insert(r);
                }
                continue;
            }
            if subtype == b"Popup" {
                continue;
            }
            if under(d) && removed.insert(r) {
                if subtype == b"Widget" {
                    if let Some((_, name)) = widget_owner.iter().find(|(w, _)| *w == r)
                        && !doomed_fields.contains(name)
                    {
                        doomed_fields.push(name.clone());
                    }
                } else {
                    report.annotations += 1;
                }
            }
        }
        // Replies (`/IRT`) to anything removed go too, and replies to those, as deleting a comment
        // does: a kept reply would otherwise keep the removed comment reachable.
        let mut replies: HashMap<ObjRef, Vec<ObjRef>> = HashMap::new();
        for r in annots.iter().filter_map(Object::as_ref) {
            if let Some(t) = doc.get(r).as_dict().and_then(|d| d.reference(b"IRT")) {
                replies.entry(t).or_default().push(r);
            }
        }
        let mut todo: Vec<ObjRef> = removed.iter().copied().collect();
        while let Some(t) = todo.pop() {
            for &r in replies.get(&t).into_iter().flatten() {
                if removed.insert(r) {
                    report.annotations += 1;
                    todo.push(r);
                }
            }
        }
        let inline_under = |a: &Object| a.as_dict().is_some_and(|d| d.name(b"Subtype") != Some(b"Popup") && under(d));
        // An inline widget has no field we can delete with it: refuse rather than keep its value.
        if annots.iter().any(|a| inline_under(a) && a.as_dict().and_then(|d| d.name(b"Subtype")) == Some(b"Widget")) {
            return Err(RedactError::StillUsed(vec![pi]));
        }
        // Pop-ups a removed annotation names go too, even without a /Parent pointing back.
        let popups: HashSet<ObjRef> = removed
            .iter()
            .filter_map(|r| doc.get(*r).as_dict().and_then(|d| d.reference(b"Popup")))
            .chain(annots.iter().filter(|a| inline_under(a)).filter_map(|a| a.as_dict()?.reference(b"Popup")))
            .collect();
        let (kept, dropped): (Vec<Object>, Vec<Object>) = annots.into_iter().partition(|a| {
            // An annotation written inline in /Annots (not as its own object) goes with the page entry.
            let Some(r) = a.as_ref() else { return !inline_under(a) };
            if removed.contains(&r) || popups.contains(&r) {
                return false;
            }
            // Pop-ups of removed annotations go too.
            let obj = doc.get(r);
            !obj.as_dict().and_then(|d| d.get(b"Parent")).and_then(Object::as_ref).is_some_and(|p| removed.contains(&p))
        });
        report.annotations += dropped.iter().filter(|a| a.as_ref().is_none()).count();
        // Nothing removed may stay reachable (step 6 checks).
        gone_annots.extend(dropped.iter().filter_map(Object::as_ref));
        doc.update_dict(page.obj, |d| {
            if kept.is_empty() {
                d.remove(b"Annots");
            } else {
                d.set(b"Annots".to_vec(), Object::Array(kept));
            }
        })?;
    }

    // A form whose marks couldn't be carried between its draws could keep, in one draw, what
    // another draw's marks removed.
    if !shared.unmapped.is_empty() {
        let mut pages: Vec<usize> =
            shared.unmapped.iter().filter_map(|f| shared.forms.get(f)).flat_map(|draws| draws.iter().map(|(page, ..)| *page)).collect();
        pages.sort_unstable();
        pages.dedup();
        return Err(RedactError::SharedForm(pages));
    }
    // The document's cleared image copies. An image wholly under a mark somewhere is blanked in
    // its copy too, so no draw anywhere keeps pixels a mark covered.
    for (original, img) in std::mem::take(&mut shared.images) {
        if img.draws > 1 || shared.gone.contains(&original) {
            report.shared_redacted += img.draws;
        }
        let stream = if shared.gone.contains(&original) {
            match image::clear(doc, &img.stream, &pdfcraft_content::Matrix::IDENTITY, &[[-1.0, -1.0, 2.0, 2.0]]) {
                Ok(Some(blank)) => blank,
                _ => return Err(RedactError::StillUsed(Vec::new())),
            }
        } else {
            img.stream
        };
        doc.set(img.copy, Object::Stream(stream));
    }

    // 3. Form fields with a widget under a mark (all their widgets go).
    for name in &doomed_fields {
        if pdfcraft_forms::fields(doc).iter().any(|f| &f.name == name) {
            pdfcraft_forms::delete_field(doc, name)?;
            report.fields += 1;
            // Its widgets on other pages and outside the marks are gone too (step 6 checks).
            gone_annots.extend(widget_owner.iter().filter(|(_, n)| n == name).map(|(w, _)| *w));
        }
    }

    // 4. The boxes.
    for &pi in &by_page {
        let page_marks: Vec<&Mark> = chosen.iter().copied().filter(|m| m.page == pi).collect();
        let content = overlay_content(&page_marks);
        if !content.is_empty() {
            pdfcraft_edit::stamp(doc, pi, "Redaction", content)?;
        }
    }

    // 5. Verify: nothing readable may remain under a region.
    let mut residue = 0;
    for &pi in &by_page {
        let page = pdfcraft_model::pages(doc).swap_remove(pi);
        let rects: Vec<[f64; 4]> = chosen.iter().filter(|m| m.page == pi).flat_map(|m| m.rects.iter().copied()).collect();
        let (list, data) = page_streams(doc, &page.dict, pi)?;
        // The overlay stream (last) draws text of its own: the overlay label is allowed.
        let original: Vec<Vec<u8>> = list
            .iter()
            .zip(data)
            .filter(|(o, _)| doc.resolve(o).as_dict().and_then(|d| d.name(b"PCMark")) != Some(b"Redaction"))
            .map(|(_, d)| d)
            .collect();
        let resources = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
        let mut scratch = Report::default();
        let mut scope = Scope::new(&rects, Mode::Verify, &mut scratch);
        residue += process(doc, &mut scope, &original, &resources, pdfcraft_content::Matrix::IDENTITY).residue;
    }
    if residue > 0 {
        return Err(RedactError::Residue(residue));
    }
    // 6. Nothing redacted may stay reachable through another page or draw. The structure tree's
    // references to removed annotations go first; any other reference fails closed.
    report.tags += tags::drop_annotations(doc, &gone_annots)?;
    retired.extend(&gone_annots);
    check_retired(doc, &retired)?;
    // The previous revision still holds the removed content: the next save must rewrite.
    doc.require_full_save();
    Ok(report)
}

/// Remove redaction marks without applying them (`None` = all).
pub fn clear_marks(doc: &mut Document, pages: Option<&[usize]>) -> Result<usize, RedactError> {
    let all = marks(doc);
    let doomed: Vec<ObjRef> = all.iter().filter(|m| pages.is_none_or(|p| p.contains(&m.page))).map(|m| m.obj).collect();
    if doomed.is_empty() {
        return Err(RedactError::NothingToApply);
    }
    for p in pdfcraft_model::pages(doc) {
        let annots = annots_of(doc, &p.dict);
        let kept: Vec<Object> = annots
            .iter()
            .filter(|a| {
                let Some(r) = a.as_ref() else { return true };
                let parent = doc.get(r).as_dict().and_then(|d| d.get(b"Parent")).and_then(Object::as_ref);
                !doomed.contains(&r) && !parent.is_some_and(|p| doomed.contains(&p))
            })
            .cloned()
            .collect();
        if kept.len() != annots.len() {
            doc.update_dict(p.obj, |d| d.set(b"Annots".to_vec(), Object::Array(kept)))?;
        }
    }
    Ok(doomed.len())
}
