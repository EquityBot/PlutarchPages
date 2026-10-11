//! Remove PDF/A and PDF/X identification: the `pdfaid:` and PDF/X version properties in the XMP
//! metadata (ISO 19005, ISO 15930) and the PDF/X keys in the document information dictionary.
//! The document content is unchanged; it simply stops claiming the standard.

use pdfcraft_cos::{Document, Object, Stream};

use crate::OptimizeError;

/// The XMP properties that claim a standard.
const PROPERTIES: [&str; 7] =
    ["pdfaid:part", "pdfaid:conformance", "pdfaid:amd", "pdfaid:rev", "pdfxid:GTS_PDFXVersion", "pdfx:GTS_PDFXVersion", "pdfx:GTS_PDFXConformance"];
/// The information dictionary keys PDF/X adds.
const INFO_KEYS: [&[u8]; 2] = [b"GTS_PDFXVersion", b"GTS_PDFXConformance"];
/// The largest XMP packet edited.
const MAX_XMP: usize = 16 << 20;

/// `xmp` without `property`, written either as an attribute (`name="v"` or `name='v'`) or as an
/// element (`<name>v</name>`). Returns the new text and how many were removed.
fn remove_property(xmp: &str, property: &str) -> (String, usize) {
    let mut out = String::with_capacity(xmp.len());
    let mut rest = xmp;
    let mut removed = 0;
    let open = format!("<{property}>");
    let close = format!("</{property}>");
    loop {
        let pattern = format!("{property}=");
        let attr = rest.match_indices(&pattern).map(|(i, _)| i).find(|&i| i == 0 || rest.get(..i).is_some_and(|b| b.ends_with(char::is_whitespace)));
        let elem = rest.find(&open);
        match (attr, elem) {
            (Some(a), e) if e.is_none_or(|e| a < e) => {
                let value_at = a + property.len() + 1;
                let quote = rest.get(value_at..).and_then(|v| v.chars().next());
                let end = match quote {
                    Some(q @ ('"' | '\'')) => rest.get(value_at + 1..).and_then(|v| v.find(q)).map(|i| value_at + 1 + i + 1),
                    _ => None,
                };
                let Some(end) = end else {
                    out.push_str(rest);
                    return (out, removed);
                };
                // Drop the whitespace before the attribute too.
                let start = rest.get(..a).map_or(a, |b| b.trim_end().len());
                out.push_str(rest.get(..start).unwrap_or_default());
                rest = rest.get(end..).unwrap_or_default();
                removed += 1;
            }
            (_, Some(e)) => {
                let Some(end) = rest.get(e..).and_then(|v| v.find(&close)).map(|i| e + i + close.len()) else {
                    out.push_str(rest);
                    return (out, removed);
                };
                out.push_str(rest.get(..e).unwrap_or_default().trim_end_matches([' ', '\t']));
                rest = rest.get(end..).unwrap_or_default();
                removed += 1;
            }
            _ => {
                out.push_str(rest);
                return (out, removed);
            }
        }
    }
}

/// Remove PDF/A and PDF/X identification from `doc`. Returns how many claims were removed.
pub(crate) fn remove_identification(doc: &mut Document) -> Result<usize, OptimizeError> {
    let mut removed = 0;
    let Some(root) = doc.root() else { return Ok(0) };
    if let Some(Object::Ref(meta)) = doc.get(root).as_dict().and_then(|c| c.get(b"Metadata").cloned())
        && let Object::Stream(s) = &*doc.get(meta)
        && let Ok(bytes) = s.decoded_within(MAX_XMP)
        && let Ok(text) = String::from_utf8(bytes)
    {
        let mut xmp = text;
        let mut n = 0;
        for p in PROPERTIES {
            let (next, k) = remove_property(&xmp, p);
            xmp = next;
            n += k;
        }
        if n > 0 {
            let mut dict = s.dict.clone();
            for k in [&b"Filter"[..], b"DecodeParms", b"Length"] {
                dict.remove(k);
            }
            doc.set(meta, Object::Stream(Stream::from_raw(dict, xmp.into_bytes())));
            removed += n;
        }
    }
    if let Some(Object::Ref(info)) = doc.trailer().get(b"Info").cloned() {
        let present = doc.get(info).as_dict().map_or(0, |d| INFO_KEYS.iter().filter(|k| d.contains(k)).count());
        if present > 0 {
            doc.update_dict(info, |d| {
                for k in INFO_KEYS {
                    d.remove(k);
                }
            })?;
            removed += present;
        }
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attributes_and_elements_are_removed() {
        let xmp = r#"<rdf:Description rdf:about="" xmlns:pdfaid="http://www.aiim.org/pdfa/ns/id/" pdfaid:part="2" pdfaid:conformance='B'><dc:title>Kept</dc:title></rdf:Description><rdf:Description><pdfaid:part>1</pdfaid:part><pdfaid:conformance>A</pdfaid:conformance></rdf:Description>"#;
        let mut text = xmp.to_string();
        let mut n = 0;
        for p in PROPERTIES {
            let (t, k) = remove_property(&text, p);
            text = t;
            n += k;
        }
        assert_eq!(n, 4);
        assert!(!text.contains("pdfaid:part") && !text.contains("pdfaid:conformance"), "{text}");
        assert!(text.contains("xmlns:pdfaid=") && text.contains("<dc:title>Kept</dc:title>"), "{text}");
    }

    #[test]
    fn odd_xmp_does_not_panic() {
        for s in ["pdfaid:part=", "pdfaid:part=\"unterminated", "<pdfaid:part>no close", "", "é pdfaid:part='1'é"] {
            let (t, _) = remove_property(s, "pdfaid:part");
            assert!(t.len() <= s.len());
        }
        assert_eq!(remove_property("xpdfaid:part=\"1\"", "pdfaid:part").1, 0, "a longer name is not the property");
    }

    #[test]
    fn a_document_stops_claiming_pdfa_and_pdfx() {
        let mut doc = Document::new_empty();
        let xmp =
            br#"<x:xmpmeta><rdf:RDF><rdf:Description pdfaid:part="3" pdfaid:conformance="B" pdfx:GTS_PDFXVersion="PDF/X-4"/></rdf:RDF></x:xmpmeta>"#;
        let mut sd = pdfcraft_cos::Dict::new();
        sd.set(b"Type".to_vec(), Object::name("Metadata"));
        sd.set(b"Subtype".to_vec(), Object::name("XML"));
        let meta = doc.add(Object::Stream(Stream::flate(sd, xmp)));
        let root = doc.root().unwrap();
        doc.update_dict(root, |c| c.set(b"Metadata".to_vec(), Object::Ref(meta))).unwrap();
        let mut info = pdfcraft_cos::Dict::new();
        info.set(b"GTS_PDFXVersion".to_vec(), pdfcraft_cos::PdfString::text("PDF/X-4"));
        let info = doc.add(info);
        doc.trailer_mut().set(b"Info".to_vec(), Object::Ref(info));
        assert_eq!(remove_identification(&mut doc).unwrap(), 4);
        let Object::Stream(s) = &*doc.get(meta) else { panic!("metadata stream") };
        assert!(!s.dict.contains(b"Filter"), "XMP stays readable");
        let text = String::from_utf8(s.decoded().unwrap()).unwrap();
        assert!(!text.contains("pdfaid") || text.contains("xmlns"), "{text}");
        assert!(!text.contains("GTS_PDFXVersion"), "{text}");
        assert!(!doc.get(info).as_dict().unwrap().contains(b"GTS_PDFXVersion"));
        assert_eq!(remove_identification(&mut doc).unwrap(), 0, "nothing left to remove");
    }
}
