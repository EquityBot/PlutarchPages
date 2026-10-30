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

#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use std::collections::{HashMap, HashSet};

use pdfcraft_cos::{Dict, Document, ObjRef, Object, Stream};

pub mod codes;
mod image;
mod interp;
mod overlay;
pub mod patterns;
pub mod sanitize;
mod tags;
#[cfg(test)]
mod tests;
pub(crate) mod type3;
pub(crate) mod type3_tables;

use interp::{Mode, Scope, process};

pub type Rgb = [f64; 3];

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RedactError {
    #[error("there are no redaction marks to apply")]
    NothingToApply,
    #[error("page {0} has content that can't be read, so it can't be redacted safely")]
    Unreadable(usize),
    /// Redact annotations with no readable area (neither usable `/QuadPoints` nor a usable `/Rect`).
    /// Skipping them would leave the text under them in the file while the others report success.
    #[error(
        "{0} redaction mark(s) have no readable area (neither usable QuadPoints nor a usable Rect), so the text under them can't be known; nothing was redacted. Remove those marks or mark the text again, then apply again"
    )]
    UnreadableMark(usize),
    #[error("redaction could not be verified: {0} item(s) were still found under the marks, so nothing was changed")]
    Residue(usize),
    /// A mark's overlay text has a character Helvetica can't draw; nothing was redacted.
    #[error("{0}")]
    Undrawable(String),
    #[error(
        "page {0} has vertical-writing text while a redaction mark is pending, and PdfCraft cannot tell whether that text lies under the mark yet, so nothing was changed"
    )]
    VerticalText(usize),
    #[error(
        "redaction appearance has invalid geometry or exceeds the 8 MiB budget; reduce the marked area, turn off repeated text or apply fewer pages at once (nothing was redacted)"
    )]
    OverlayLimit,
    /// The structure tree couldn't be cleaned completely (#3302): an element written in place that
    /// would need changing, an element whose inherited page can't be found, or a tree too large to
    /// walk. Tags that kept the removed text or annotation would undo the redaction.
    #[error(
        "the document's tags can't be cleaned completely (an element written in place, or a structure tree too large or too deep to follow); the operation was refused"
    )]
    Tags,
    /// Objects written in place nest deeper than hidden information is followed, so its removal
    /// could not be shown complete.
    #[error("the document's objects nest too deeply to remove hidden information completely; the operation was refused")]
    TooDeep,
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
    pub replacement_text: usize,
    /// Structure elements that lost alternate/actual text or emptied marked content, and
    /// structure references to removed annotations.
    pub tags: usize,
    /// The form was an AcroForm+XFA form and its `/XFA` packets (which still held redacted field
    /// values, and from which a relayout could rebuild them) were removed.
    pub xfa_removed: bool,
    /// The content streams this apply added to draw the marks' boxes and labels. They are the
    /// only streams a check for text left under the marks may skip: an input stream labelled
    /// like an overlay (`/PCMark /Redaction`) is checked like any other.
    pub overlays: Vec<ObjRef>,
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

/// Every annotation the pages list. Compared before and after a removal, what is no longer listed
/// leaves the structure tree too ([`tags::drop_object_refs`]).
pub(crate) fn listed_annotations(doc: &Document) -> HashSet<ObjRef> {
    pdfcraft_model::pages(doc).iter().flat_map(|p| annots_of(doc, &p.dict)).filter_map(|a| a.as_ref()).collect()
}

/// Every redaction mark in the document, page by page.
pub fn marks(doc: &Document) -> Vec<Mark> {
    marks_checked(doc).0
}

/// [`marks`], and the 0-based page of each Redact annotation that has no readable area, which
/// [`apply`] refuses rather than skipping.
pub(crate) fn marks_checked(doc: &Document) -> (Vec<Mark>, Vec<usize>) {
    let (mut out, mut unreadable) = (Vec::new(), Vec::new());
    for (pi, p) in pdfcraft_model::pages(doc).iter().enumerate() {
        for a in annots_of(doc, &p.dict) {
            let Some(r) = a.as_ref() else {
                // A mark written in place has no object to find, apply or remove: refuse it
                // rather than apply the others as if it were not there.
                if doc.resolve(&a).as_dict().is_some_and(|d| d.name(b"Subtype") == Some(b"Redact")) {
                    unreadable.push(pi);
                }
                continue;
            };
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
                unreadable.push(pi);
                continue;
            }
            let overlay = d.get(b"OverlayText").and_then(|t| doc.resolve(t).as_string().map(|s| s.to_text())).unwrap_or_default();
            out.push(Mark { page: pi, obj: r, rects, fill: color(doc, d, b"IC"), overlay, look: overlay_look(doc, d) });
        }
    }
    (out, unreadable)
}

/// A page's content streams: (the references or inline objects as listed, their decoded data),
/// decoded within one page's [`pdfcraft_cos::DecodeBudget`].
pub(crate) fn page_streams(doc: &Document, page: &Dict, index: usize) -> Result<(Vec<Object>, Vec<Vec<u8>>), RedactError> {
    page_streams_within(doc, page, index, &mut pdfcraft_cos::DecodeBudget::page())
}

/// [`page_streams`], decoded against `budget`, which the walk of the page's forms then goes on
/// spending.
pub(crate) fn page_streams_within(
    doc: &Document,
    page: &Dict,
    index: usize,
    budget: &mut pdfcraft_cos::DecodeBudget,
) -> Result<(Vec<Object>, Vec<Vec<u8>>), RedactError> {
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
            // Strict: a tolerant decode keeps what precedes corruption and drops the rest, which
            // still draws in viewers that read further; anything not read can't be cleaned.
            Object::Stream(s) => data.push(budget.decode_strict(s).map_err(|_| RedactError::Unreadable(index + 1))?),
            Object::Null => data.push(Vec::new()),
            _ => return Err(RedactError::Unreadable(index + 1)),
        }
    }
    Ok((list, data))
}

/// The nearest ancestor of `page` that holds `/Resources` (where an inheriting page gets them):
/// `Some(None)` when none does, `None` when the chain can't be followed (a cycle, a broken node or
/// a tree deeper than any real one), so callers can stay conservative.
fn resource_source(doc: &Document, page: ObjRef) -> Option<Option<ObjRef>> {
    let mut seen = HashSet::new();
    let mut parent = doc.get(page).as_dict().and_then(|d| d.get(b"Parent")).and_then(Object::as_ref);
    while let Some(r) = parent {
        if !seen.insert(r) || seen.len() > 4096 {
            return None;
        }
        let node = doc.get(r);
        let d = node.as_dict()?;
        if d.contains(b"Resources") {
            return Some(Some(r));
        }
        parent = d.get(b"Parent").and_then(Object::as_ref);
    }
    Some(None)
}

/// Before rewriting `selected` pages, pin the resources they inherit onto them, and retire the
/// ancestor `/Resources` they came from once no other page still inherits it, so the parent can't
/// keep a removed XObject reachable. Pages that aren't redacted are left untouched; an inherited
/// dictionary that is direct becomes one shared indirect object, never a copy per page.
fn localize_resources(doc: &mut Document, selected: &[usize]) -> Result<(), pdfcraft_cos::CosError> {
    let pages = pdfcraft_model::pages(doc);
    let inherits = |doc: &Document, p: &pdfcraft_model::Page| doc.get(p.obj).as_dict().is_some_and(|d| !d.contains(b"Resources"));
    // Ancestor → the reference its resources are shared through, and whether that reference is new.
    let mut shared: HashMap<ObjRef, (Object, bool)> = HashMap::new();
    for page in selected.iter().filter_map(|&i| pages.get(i)) {
        // What the page tree gives the page; nothing to pin if it gets nothing.
        let Some(effective) = page.dict.get(b"Resources").cloned() else { continue };
        if !inherits(doc, page) {
            continue;
        }
        let source = resource_source(doc, page.obj).flatten();
        let original = source.and_then(|s| doc.get(s).as_dict().and_then(|d| d.get(b"Resources").cloned()));
        let value = match source.filter(|_| original.as_ref() == Some(&effective)) {
            // `/Parent` agrees with the page tree: share one reference per ancestor.
            Some(source) => match shared.get(&source) {
                Some((v, _)) => v.clone(),
                None => {
                    // A direct dictionary is made indirect once, so every pinned page shares it.
                    let (v, new) = match effective {
                        Object::Ref(_) => (effective, false),
                        direct => (Object::Ref(doc.add(direct)), true),
                    };
                    shared.insert(source, (v.clone(), new));
                    v
                }
            },
            // A malformed tree whose `/Parent` disagrees: pin this page alone and retire nothing.
            None => match effective {
                Object::Ref(_) => effective,
                direct => Object::Ref(doc.add(direct)),
            },
        };
        doc.update_dict(page.obj, |d| d.set(b"Resources".to_vec(), value))?;
    }
    if shared.is_empty() {
        return Ok(());
    }
    // Ancestors some page still inherits from keep their resources (that page draws them), through
    // the shared reference; the others are retired. A page that inherits resources through a chain
    // that can't be followed might inherit from any of them, so then none is retired.
    let mut still_used: HashSet<ObjRef> = HashSet::new();
    for page in pages.iter().filter(|p| p.dict.contains(b"Resources") && inherits(doc, p)) {
        match resource_source(doc, page.obj) {
            Some(Some(source)) => {
                if shared.contains_key(&source) {
                    still_used.insert(source);
                }
            }
            _ => still_used.extend(shared.keys().copied()),
        }
    }
    for (source, (value, new)) in shared {
        if still_used.contains(&source) {
            if !new {
                continue;
            }
            doc.update_dict(source, |d| d.set(b"Resources".to_vec(), value))?;
        } else {
            doc.update_dict(source, |d| {
                d.remove(b"Resources");
            })?;
        }
    }
    Ok(())
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

/// What a removed annotation may hold as separate objects that must go with it.
const PAYLOAD_KEYS: [&[u8]; 4] = [b"Contents", b"RC", b"A", b"Popup"];

/// The separate objects a removed annotation holds: [`PAYLOAD_KEYS`] and its appearance streams.
/// A kept annotation sharing an appearance would draw the redacted content outside the marks, so
/// this makes apply refuse and name its pages, as it does for an image drawn elsewhere. The cost is
/// a refusal when identical icons share one, until every use is marked.
fn annotation_payloads(doc: &Document, d: &Dict) -> Vec<ObjRef> {
    let mut held: Vec<ObjRef> = PAYLOAD_KEYS.iter().filter_map(|k| d.reference(k)).collect();
    let ap = d.get(b"AP").and_then(|ap| doc.resolve(ap).as_dict().cloned()).unwrap_or_default();
    for (_, state) in ap.iter() {
        held.extend(state.as_ref());
        // One stream per appearance state (/N << /On 5 0 R /Off 6 0 R >>).
        let states = doc.resolve(state);
        if !matches!(*states, Object::Stream(_))
            && let Some(states) = states.as_dict()
        {
            held.extend(states.iter().filter_map(|(_, s)| s.as_ref()));
        }
    }
    held
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
/// file; callers keep the previous document for undo.
///
/// # Errors
///
/// On any error `doc` is left as it was: the redaction runs on a copy (cheap: unchanged objects
/// are shared) that replaces `doc` only on success.
pub fn apply(doc: &mut Document, pages: Option<&[usize]>) -> Result<Report, RedactError> {
    let mut work = doc.clone();
    let report = apply_to(&mut work, pages)?;
    *doc = work;
    Ok(report)
}

/// [`apply`] without the copy: an error can leave `doc` part redacted.
fn apply_to(doc: &mut Document, pages: Option<&[usize]>) -> Result<Report, RedactError> {
    let (all_marks, unreadable) = marks_checked(doc);
    // A mark with no readable area is not applied, and applying the others would look complete:
    // refuse before anything changes. Only the pages being applied count.
    let unreadable = unreadable.iter().filter(|page| pages.is_none_or(|p| p.contains(page))).count();
    if unreadable > 0 {
        return Err(RedactError::UnreadableMark(unreadable));
    }
    let chosen: Vec<&Mark> = all_marks.iter().filter(|m| pages.is_none_or(|p| p.contains(&m.page))).collect();
    if chosen.is_empty() {
        return Err(RedactError::NothingToApply);
    }
    // Overlay text is drawn in Helvetica; applying can't be undone in the file, so text it can't
    // draw (a mark another program made) is refused before anything changes, never burnt in as `?`.
    if let Some((m, c)) = chosen.iter().find_map(|m| pdfcraft_fonts::first_undrawable(&m.overlay).map(|c| (m, c))) {
        return Err(RedactError::Undrawable(format!(
            "{}; change or clear that mark's overlay text, then apply again (nothing was redacted)",
            pdfcraft_fonts::undrawable_message(&format!("the overlay text of a redaction mark on page {}", m.page + 1), c)
        )));
    }
    let mut report = Report { marks: chosen.len(), ..Report::default() };
    // `marks` visits pages in order. Keep slices of that order for appearance generation,
    // content rewriting and verification instead of scanning all marks for every page.
    let groups: Vec<_> = chosen.chunk_by(|a, b| a.page == b.page).filter_map(|group| group.first().map(|mark| (mark.page, group))).collect();
    let by_page: Vec<_> = groups.iter().map(|(page, _)| *page).collect();
    // Budget failures must leave even a direct library caller's document untouched.
    let mut remaining = overlay::MAX_BYTES;
    let overlays = groups
        .iter()
        .map(|&(pi, page_marks)| overlay::content(page_marks, &mut remaining).map(|content| (pi, content)))
        .collect::<Result<Vec<_>, _>>()?;
    report.pages = by_page.len();
    localize_resources(doc, &by_page)?;
    let widget_owner: Vec<(ObjRef, ObjRef)> =
        pdfcraft_forms::fields(doc).into_iter().flat_map(|f| f.widgets.iter().map(|w| (w.obj, f.obj)).collect::<Vec<_>>()).collect();
    let listed_before = listed_annotations(doc);
    let mut applying = Applying {
        report,
        doomed_fields: Vec::new(),
        doomed_xobjects: Vec::new(),
        retired_properties: Vec::new(),
        widget_owner,
        removed_everywhere: HashSet::new(),
        shared: interp::Shared { collect: true, ..Default::default() },
        retired: HashSet::new(),
        gone_payloads: HashSet::new(),
    };
    // 0. Where each form XObject is drawn over the marks, on every page, so each such draw is
    // redacted with the marks of all of them (changes nothing).
    for &(pi, page_marks) in &groups {
        let page = pdfcraft_model::pages(doc).swap_remove(pi);
        let rects: Vec<[f64; 4]> = page_marks.iter().flat_map(|m| m.rects.iter().copied()).collect();
        let mut budget = pdfcraft_cos::DecodeBudget::page();
        let (_, data) = page_streams_within(doc, &page.dict, pi, &mut budget)?;
        let resources = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
        let mut scratch = Report::default();
        let mut scope = Scope::new(&rects, Mode::Verify, &mut scratch, budget);
        scope.page = pi;
        scope.shared = std::mem::take(&mut applying.shared);
        process(doc, &mut scope, &data, &resources, pdfcraft_content::Matrix::IDENTITY);
        applying.shared = std::mem::take(&mut scope.shared);
    }
    applying.shared.collect = false;
    for &(pi, page_marks) in &groups {
        let page = pdfcraft_model::pages(doc).swap_remove(pi);
        let rects: Vec<[f64; 4]> = page_marks.iter().flat_map(|m| m.rects.iter().copied()).collect();
        redact_page_content(doc, &mut applying, pi, &page, &rects)?;
        remove_page_annotations(doc, &mut applying, pi, &page, &rects, page_marks)?;
    }
    let Applying {
        mut report,
        doomed_fields,
        doomed_xobjects,
        retired_properties,
        mut removed_everywhere,
        widget_owner,
        shared,
        mut retired,
        mut gone_payloads,
    } = applying;

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
    for (original, img) in shared.images {
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

    // 2b. Replies and pop-ups of a removed annotation can be listed on another page than their
    // parent: their `/IRT` or `/Parent` would keep the removed comment reachable.
    loop {
        let mut grew = false;
        for p in pdfcraft_model::pages(doc) {
            let annots = annots_of(doc, &p.dict);
            let mut kept = Vec::with_capacity(annots.len());
            for a in &annots {
                let Some(r) = a.as_ref() else {
                    kept.push(a.clone());
                    continue;
                };
                let obj = doc.get(r);
                let Some(d) = obj.as_dict() else {
                    kept.push(a.clone());
                    continue;
                };
                let is_reply =
                    d.get(b"IRT").and_then(Object::as_ref).is_some_and(|x| removed_everywhere.contains(&x)) && d.name(b"Subtype") != Some(b"Widget");
                let is_popup = d.get(b"Parent").and_then(Object::as_ref).is_some_and(|x| removed_everywhere.contains(&x));
                if removed_everywhere.contains(&r) || is_reply || is_popup {
                    if removed_everywhere.insert(r) {
                        grew = true;
                        if is_reply {
                            report.annotations += 1;
                        }
                    }
                } else {
                    kept.push(a.clone());
                }
            }
            if kept.len() != annots.len() {
                doc.update_dict(p.obj, |d| {
                    if kept.is_empty() {
                        d.remove(b"Annots");
                    } else {
                        d.set(b"Annots".to_vec(), Object::Array(kept));
                    }
                })?;
            }
        }
        if !grew {
            break;
        }
    }

    // 3. Form fields with a widget under a mark (all their widgets go).
    for field in &doomed_fields {
        if pdfcraft_forms::fields(doc).iter().any(|f| f.obj == *field) {
            pdfcraft_forms::delete_field_object(doc, *field)?;
            report.fields += 1;
        }
    }

    // 3a. A tagged PDF's structure tree still points at the removed comments and widgets
    // (`/OBJR`, #2874): drop those references, or a full rewrite keeps them reachable, the
    // comment text and the field value included. Widgets of a removed field on other pages count.
    let mut gone: HashSet<ObjRef> = listed_before.difference(&listed_annotations(doc)).copied().collect();
    gone.extend(widget_owner.iter().filter(|(_, f)| doomed_fields.contains(f)).map(|(w, _)| *w));
    report.tags += tags::drop_object_refs(doc, &gone)?;

    // 3b. An XFA form mirrors its field values in the /XFA datasets packet, and a relayout
    // rebuilds fields and pages from that packet, so deleting the AcroForm field alone leaves the
    // redacted value in the file and lets a later relayout put it back. Once redactions are
    // applied the form stops being XFA: the packets, /NeedsRendering, the layout mark and the
    // rest go (pdfcraft_xfa::strip), leaving a plain already-laid-out AcroForm that neither the
    // saved file nor a relayout can restore from.
    if pdfcraft_xfa::xfa_shape(doc).is_some() {
        let r = pdfcraft_xfa::strip(doc).map_err(|e| pdfcraft_edit::EditError::Invalid(e.to_string()))?;
        report.xfa_removed = !r.packets.is_empty();
    }

    // 4. The boxes.
    for (pi, content) in overlays {
        if !content.is_empty() {
            report.overlays.push(pdfcraft_edit::stamp(doc, pi, "Redaction", content)?);
        }
    }

    // A removed image or form must not stay reachable through another resource dictionary (an
    // inherited or shared /Resources): a full save keeps whatever is still referenced.
    interp::unlink_doomed(doc, &doomed_xobjects, &retired_properties)?;

    // 5. Verify: nothing readable may remain under a region.
    let mut residue = 0;
    for &(pi, page_marks) in &groups {
        let page = pdfcraft_model::pages(doc).swap_remove(pi);
        let rects: Vec<[f64; 4]> = page_marks.iter().flat_map(|m| m.rects.iter().copied()).collect();
        let mut budget = pdfcraft_cos::DecodeBudget::page();
        let (list, data) = page_streams_within(doc, &page.dict, pi, &mut budget)?;
        // The overlay stream draws text of its own: the overlay label is allowed.
        let original: Vec<Vec<u8>> =
            list.iter().zip(data).filter(|(o, _)| !o.as_ref().is_some_and(|r| report.overlays.contains(&r))).map(|(_, d)| d).collect();
        let resources = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
        let mut scratch = Report::default();
        let mut scope = Scope::new(&rects, Mode::Verify, &mut scratch, budget);
        residue += process(doc, &mut scope, &original, &resources, pdfcraft_content::Matrix::IDENTITY).residue;
    }
    if residue > 0 {
        return Err(RedactError::Residue(residue));
    }
    // 6. Nothing redacted may stay reachable through another page or draw. Text, actions and
    // pop-ups removed annotations hold as separate objects count too: if anything kept shares
    // one, apply fails closed, as it does for a redacted image drawn elsewhere.
    for r in &gone {
        let Some(d) = doc.get(*r).as_dict().cloned() else { continue };
        gone_payloads.extend(annotation_payloads(doc, &d));
    }
    retired.extend(&doomed_xobjects);
    retired.extend(&gone);
    retired.extend(&gone_payloads);
    check_retired(doc, &retired)?;
    // The previous revision still holds the removed content: the next save must rewrite.
    doc.require_full_save();
    Ok(report)
}

/// What applying the marks collects across pages.
struct Applying {
    report: Report,
    doomed_fields: Vec<ObjRef>,
    /// The original image and form XObjects the rewrite drops or replaces, on any page.
    doomed_xobjects: Vec<ObjRef>,
    retired_properties: Vec<Object>,
    widget_owner: Vec<(ObjRef, ObjRef)>,
    removed_everywhere: HashSet<ObjRef>,
    /// Images and forms shared between draws (see [`interp::Shared`]).
    shared: interp::Shared,
    /// Original content streams the rewrite replaced: another page may still use them.
    retired: HashSet<ObjRef>,
    /// What removed annotations hold as separate objects (text, actions, named pop-ups).
    gone_payloads: HashSet<ObjRef>,
}

/// Step 1 for one page: rewrites its content so nothing readable stays under a mark, and cleans the tags it no longer shows.
fn redact_page_content(
    doc: &mut Document,
    applying: &mut Applying,
    pi: usize,
    page: &pdfcraft_model::Page,
    rects: &[[f64; 4]],
) -> Result<(), RedactError> {
    let Applying { report, doomed_xobjects, retired_properties, shared, retired, .. } = applying;
    // 1. Content.
    let mut budget = pdfcraft_cos::DecodeBudget::page();
    let (list, data) = page_streams_within(doc, &page.dict, pi, &mut budget)?;
    let resources = page.dict.get(b"Resources").and_then(|r| doc.resolve(r).as_dict().cloned()).unwrap_or_default();
    let out = {
        let mut scope = Scope::new(rects, Mode::Apply, report, budget);
        scope.page = pi;
        scope.shared = std::mem::take(shared);
        let out = process(doc, &mut scope, &data, &resources, pdfcraft_content::Matrix::IDENTITY);
        *shared = std::mem::take(&mut scope.shared);
        out
    };
    // Fail closed: the interpreter lays vertical-writing text out horizontally, so it cannot
    // tell whether those glyphs lie under a mark. Rather than report success and possibly
    // leave the text in place, refuse the whole operation (the caller keeps the previous
    // document). The error does not claim the text was under the mark. Full vertical-text
    // removal is a future enhancement.
    if let Some(error) = out.error {
        return Err(error.into());
    }
    if out.vertical {
        return Err(RedactError::VerticalText(pi + 1));
    }
    // An inline image the renderer reads no image for (no `EI` it accepts, or parameters it
    // can't read): where the page's operators resume after it is a guess, so neither removal
    // nor the verify pass can be trusted on this page.
    if out.ambiguous {
        return Err(RedactError::Unreadable(pi + 1));
    }
    doomed_xobjects.extend_from_slice(&out.doomed);
    retired_properties.extend(out.retired_properties);
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
    if changed || !out.xobjects.is_empty() || !out.replaced.is_empty() {
        let mut res = resources.clone();
        if !out.xobjects.is_empty() || !out.replaced.is_empty() {
            let mut xo = res.get(b"XObject").and_then(|x| doc.resolve(x).as_dict().cloned()).unwrap_or_default();
            // A full save keeps whatever is still referenced: unlink the originals this page no
            // longer draws so the removed image or form bytes cannot survive in the saved file.
            let page_content: Vec<Vec<u8>> = new_list
                .iter()
                .map(|o| match &*doc.resolve(o) {
                    Object::Stream(s) => s.decoded().unwrap_or_default(),
                    _ => Vec::new(),
                })
                .collect();
            interp::prune(&mut xo, &out.replaced, &page_content);
            for (n, r) in &out.xobjects {
                xo.set(n.clone(), Object::Ref(*r));
            }
            res.set(b"XObject".to_vec(), Object::Dict(xo));
        }
        let (_, cleaned_data) = page_streams(
            doc,
            &{
                let mut cleaned_page = page.dict.clone();
                cleaned_page.set(b"Contents".to_vec(), Object::Array(new_list.clone()));
                cleaned_page
            },
            pi,
        )?;
        retired_properties.extend(interp::prune_properties(doc, &mut res, &cleaned_data)?);
        type3::prune_type3_from_resources(doc, &mut res, &cleaned_data)?;
        doc.update_dict(page.obj, |d| {
            d.set(b"Contents".to_vec(), Object::Array(new_list));
            d.set(b"Resources".to_vec(), Object::Dict(res));
        })?;
        // Tags must not keep what the page no longer shows.
        let after = doc.get(page.obj).as_dict().cloned().unwrap_or_default();
        let (_, new_data) = page_streams(doc, &after, pi)?;
        let (changed_ids, empty_ids) =
            tags::touched(doc, &resources, pdfcraft_content::Pieces::join(&data).data(), pdfcraft_content::Pieces::join(&new_data).data());
        report.tags += tags::clean(doc, page.obj, None, &changed_ids, &empty_ids)?;
    }

    for (old, new, changed, empty) in out.tag_changes {
        report.tags += tags::clean(doc, page.obj, Some((old, new)), &changed, &empty)?;
    }
    Ok(())
}

/// Step 2 for one page: the marks, and whatever lies under them (with their pop-ups).
fn remove_page_annotations(
    doc: &mut Document,
    applying: &mut Applying,
    pi: usize,
    page: &pdfcraft_model::Page,
    rects: &[[f64; 4]],
    page_marks: &[&Mark],
) -> Result<(), RedactError> {
    let Applying { report, doomed_fields, widget_owner, removed_everywhere, gone_payloads, .. } = applying;
    // 2. Annotations: the marks, and whatever lies under them (with their pop-ups).
    let annots = annots_of(doc, &doc.get(page.obj).as_dict().cloned().unwrap_or_default());
    let mut removed: Vec<ObjRef> = Vec::new();
    let under = |d: &Dict| rect_of(doc, d.get(b"Rect")).is_some_and(|b| rects.iter().any(|x| pdfcraft_content::overlaps(*x, b, 0.0)));
    for a in &annots {
        let Some(r) = a.as_ref() else { continue };
        let obj = doc.get(r);
        let Some(d) = obj.as_dict() else { continue };
        let subtype = d.name(b"Subtype").unwrap_or(b"");
        if subtype == b"Redact" {
            if page_marks.iter().any(|m| m.obj == r) {
                removed.push(r);
            }
            continue;
        }
        if subtype == b"Popup" {
            continue;
        }
        if under(d) {
            removed.push(r);
            if subtype == b"Widget" {
                if let Some((_, field)) = widget_owner.iter().find(|(w, _)| *w == r)
                    && !doomed_fields.contains(field)
                {
                    doomed_fields.push(*field);
                }
            } else {
                report.annotations += 1;
            }
        }
    }
    // Replies (`/IRT`, ISO 32000-2 §12.5.6.2) to a removed annotation go too, and replies to
    // those: a reply placed elsewhere would keep its parent — the comment and the QuadPoints
    // of the redacted words — in the file through its `/IRT` (#2659). Each pass removes at
    // least one more annotation of this page or stops, so it ends.
    let replies: Vec<(ObjRef, ObjRef)> = annots
        .iter()
        .filter_map(Object::as_ref)
        .filter_map(|r| {
            let obj = doc.get(r);
            let d = obj.as_dict()?;
            (d.name(b"Subtype") != Some(b"Widget")).then_some(())?;
            Some((r, d.get(b"IRT").and_then(Object::as_ref)?))
        })
        .collect();
    loop {
        let before = removed.len();
        for (r, parent) in &replies {
            if !removed.contains(r) && removed.contains(parent) {
                removed.push(*r);
                report.annotations += 1;
            }
        }
        if removed.len() == before {
            break;
        }
    }
    removed_everywhere.extend(removed.iter().copied());
    // Pop-ups dropped with their parent below count too: a reply on another page may answer one.
    for a in &annots {
        let Some(r) = a.as_ref() else { continue };
        if doc.get(r).as_dict().and_then(|d| d.get(b"Parent")).and_then(Object::as_ref).is_some_and(|p| removed.contains(&p)) {
            removed_everywhere.insert(r);
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
        if removed.contains(&r) || popups.contains(&r) && doc.get(r).as_dict().and_then(|d| d.name(b"Subtype")) == Some(b"Popup") {
            return false;
        }
        // Pop-ups of removed annotations go too.
        let obj = doc.get(r);
        !obj.as_dict().and_then(|d| d.get(b"Parent")).and_then(Object::as_ref).is_some_and(|p| removed.contains(&p))
    });
    report.annotations += dropped.iter().filter(|a| a.as_ref().is_none()).count();
    for a in &dropped {
        let d = a.as_ref().map_or_else(|| a.as_dict().cloned(), |r| doc.get(r).as_dict().cloned());
        gone_payloads.extend(d.iter().flat_map(|d| annotation_payloads(doc, d)));
    }
    removed_everywhere.extend(dropped.iter().filter_map(Object::as_ref));
    doc.update_dict(page.obj, |d| {
        if kept.is_empty() {
            d.remove(b"Annots");
        } else {
            d.set(b"Annots".to_vec(), Object::Array(kept));
        }
    })?;
    Ok(())
}

/// Remove redaction marks without applying them (`None` = all), with their replies and pop-ups as
/// deleting a comment does: a reply left behind would keep its mark in the file through `/IRT`.
/// Returns how many marks went.
pub fn clear_marks(doc: &mut Document, pages: Option<&[usize]>) -> Result<usize, RedactError> {
    let targets: HashSet<ObjRef> = marks(doc).iter().filter(|m| pages.is_none_or(|p| p.contains(&m.page))).map(|m| m.obj).collect();
    if targets.is_empty() {
        return Err(RedactError::NothingToApply);
    }
    let listed: Vec<Object> = pdfcraft_model::pages(doc).iter().flat_map(|p| annots_of(doc, &p.dict)).collect();
    let doomed = pdfcraft_annot::with_replies_and_popups(doc, &listed, targets.iter().copied());
    for p in pdfcraft_model::pages(doc) {
        let annots = annots_of(doc, &p.dict);
        let kept: Vec<Object> = annots.iter().filter(|a| !pdfcraft_annot::goes_with(a, &doomed)).cloned().collect();
        // As deleting a comment does: a page whose marks were all it had loses `/Annots` again,
        // and an indirect array stays the one object the page points to.
        if kept.len() != annots.len() {
            pdfcraft_annot::set_annots(doc, p.obj, kept)?;
        }
    }
    Ok(targets.len())
}
