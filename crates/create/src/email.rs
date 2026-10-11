//! Email messages (`.eml`, RFC 5322 with MIME) to PDF: the subject, sender, recipients and date,
//! then the text of the message. An HTML-only message is converted to plain text here; nothing it
//! refers to (remote images, style sheets, tracking pixels) is ever fetched. Attachments are
//! listed and, when asked, embedded as PDF file attachments (ISO 32000-2 §7.11.4).

use mail_parser::{Address, MessageParser, MimeHeaders};
use pdfcraft_cos::{Dict, Document, Object, PdfString, Stream};

use crate::CreateError;
use crate::flow::{Block, Face, PageSetup, Para, Span, render};

/// The most attachments listed or embedded.
const MAX_ATTACHMENTS: usize = 1000;
/// The most addresses shown per header; the rest are counted.
const MAX_ADDRESSES: usize = 50;
/// The most text bodies of one message laid out.
const MAX_BODIES: usize = 20;

/// What a converted message held.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EmailReport {
    pub subject: String,
    /// Every attachment's (sanitised) file name, in message order.
    pub attachments: Vec<String>,
    /// Whether they were embedded in the PDF.
    pub embedded: bool,
    /// Characters the standard fonts can't show, written as `?`.
    pub replaced_characters: usize,
}

fn addresses(a: Option<&Address<'_>>) -> Option<String> {
    let a = a?;
    let all: Vec<String> = a
        .iter()
        .map(|x| match (x.name().map(str::trim).filter(|n| !n.is_empty()), x.address()) {
            (Some(n), Some(addr)) => format!("{n} <{addr}>"),
            (Some(n), None) => n.to_string(),
            (None, Some(addr)) => addr.to_string(),
            (None, None) => String::new(),
        })
        .filter(|s| !s.is_empty())
        .collect();
    if all.is_empty() {
        return None;
    }
    let shown = all.iter().take(MAX_ADDRESSES).cloned().collect::<Vec<_>>().join(", ");
    Some(match all.len().saturating_sub(MAX_ADDRESSES) {
        0 => shown,
        more => format!("{shown} and {more} more"),
    })
}

/// A file name safe to show and to use as an attachment name: the last path component, no
/// control characters, at most 200 characters, never empty.
pub(crate) fn safe_file_name(name: Option<&str>, fallback: &str) -> String {
    let base = name.unwrap_or_default().rsplit(['/', '\\']).next().unwrap_or_default();
    let cleaned: String = base.chars().filter(|c| !c.is_control()).take(200).collect();
    let cleaned = cleaned.trim().trim_start_matches('.').trim().to_string();
    if cleaned.is_empty() { fallback.to_string() } else { cleaned }
}

fn size_text(n: usize) -> String {
    match n {
        0..1024 => format!("{n} bytes"),
        1024..1_048_576 => format!("{:.1} KB", n as f64 / 1024.0),
        _ => format!("{:.1} MB", n as f64 / 1_048_576.0),
    }
}

/// A PDF of the email message in `bytes`, titled `title` unless the message has a subject. With
/// `embed_attachments`, its attachments are embedded as file attachments too.
pub fn from_email(title: &str, bytes: &[u8], page: (f64, f64), embed_attachments: bool) -> Result<(Document, EmailReport), CreateError> {
    let msg = MessageParser::default().parse(bytes).ok_or_else(|| CreateError::Invalid("this is not an email message".into()))?;
    if msg.headers().is_empty() {
        return Err(CreateError::Invalid("this is not an email message (no headers)".into()));
    }
    let subject = msg.subject().map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    let mut blocks = vec![Block::Para(Para {
        before: 0.0,
        after: 8.0,
        keep_with_next: true,
        ..Para::body(vec![Span::new(subject.clone().unwrap_or_else(|| "(no subject)".into()), Face::Bold)], 15.0)
    })];
    let date = msg.date().map(|d| d.to_rfc822());
    for (label, value) in [
        ("From", addresses(msg.from())),
        ("To", addresses(msg.to())),
        ("Cc", addresses(msg.cc())),
        ("Reply-To", addresses(msg.reply_to())),
        ("Date", date),
    ] {
        if let Some(v) = value {
            let p = Para::body(vec![Span::new(format!("{label}: "), Face::Bold), Span::new(v, Face::Regular)], 9.5);
            blocks.push(Block::Para(Para { after: 2.0, ..p }));
        }
    }
    blocks.push(Block::Space(4.0));
    blocks.push(Block::Rule);
    let mut any_body = false;
    for i in 0..msg.text_body_count().min(MAX_BODIES) {
        let Some(text) = msg.body_text(i) else { continue };
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        if text.trim().is_empty() {
            continue;
        }
        if any_body {
            blocks.push(Block::Rule);
        }
        any_body = true;
        blocks.push(Block::Para(Para::body(vec![Span::new(text.trim_end(), Face::Regular)], 10.5)));
    }
    if !any_body {
        blocks.push(Block::Para(Para::body(vec![Span::new("(This message has no text.)", Face::Italic)], 10.5)));
    }

    let mut files: Vec<(String, Vec<u8>, String)> = Vec::new();
    for (i, part) in msg.attachments().take(MAX_ATTACHMENTS).enumerate() {
        let fallback = if part.is_message() { format!("message-{}.eml", i + 1) } else { format!("attachment-{}", i + 1) };
        let name = safe_file_name(part.attachment_name(), &fallback);
        let kind = part.content_type().map(|ct| match ct.subtype() {
            Some(sub) => format!("{}/{}", ct.ctype(), sub),
            None => ct.ctype().to_string(),
        });
        files.push((name, part.contents().to_vec(), kind.unwrap_or_default()));
    }
    if !files.is_empty() {
        blocks.push(Block::Space(8.0));
        blocks.push(Block::Para(Para { keep_with_next: true, ..Para::body(vec![Span::new("Attachments", Face::Bold)], 11.0) }));
        let note = if embed_attachments { "embedded in this PDF" } else { "not included" };
        for (name, data, kind) in &files {
            let detail = if kind.is_empty() { size_text(data.len()) } else { format!("{}, {kind}", size_text(data.len())) };
            blocks.push(Block::Para(Para {
                indent: 18.0,
                marker: Some("\u{2022}".into()),
                after: 2.0,
                ..Para::body(vec![Span::new(format!("{name} "), Face::Regular), Span::new(format!("({detail}; {note})"), Face::Italic)], 9.5)
            }));
        }
    }

    let doc_title = subject.clone().unwrap_or_else(|| title.to_string());
    let (mut doc, replaced_characters) = render(&doc_title, &blocks, PageSetup::portrait(page))?;
    if embed_attachments && !files.is_empty() {
        embed(&mut doc, &files)?;
    }
    let report = EmailReport {
        subject: subject.unwrap_or_default(),
        attachments: files.into_iter().map(|(n, ..)| n).collect(),
        embedded: embed_attachments,
        replaced_characters,
    };
    Ok((doc, report))
}

/// Add `files` (name, data, MIME type) as the document's file attachments: a flat, sorted
/// `/EmbeddedFiles` name tree; repeated names get " (2)", " (3)"…
fn embed(doc: &mut Document, files: &[(String, Vec<u8>, String)]) -> Result<(), CreateError> {
    let mut seen = std::collections::HashSet::new();
    let mut entries: Vec<(Vec<u8>, Object)> = Vec::new();
    for (name, data, kind) in files {
        let mut unique = name.clone();
        let mut n = 2;
        while !seen.insert(unique.clone()) {
            unique = format!("{name} ({n})");
            n += 1;
        }
        let mut params = Dict::new();
        params.set(b"Size".to_vec(), Object::Int(i64::try_from(data.len()).unwrap_or(i64::MAX)));
        let mut sd = Dict::new();
        sd.set(b"Type".to_vec(), Object::name("EmbeddedFile"));
        sd.set(b"Params".to_vec(), Object::Dict(params));
        let stream = doc.add(Object::Stream(Stream::flate(sd, data)));
        let mut ef = Dict::new();
        ef.set(b"F".to_vec(), Object::Ref(stream));
        ef.set(b"UF".to_vec(), Object::Ref(stream));
        let mut spec = Dict::new();
        spec.set(b"Type".to_vec(), Object::name("Filespec"));
        spec.set(b"F".to_vec(), PdfString::text(&unique));
        spec.set(b"UF".to_vec(), PdfString::text(&unique));
        spec.set(b"EF".to_vec(), Object::Dict(ef));
        if !kind.is_empty() {
            spec.set(b"Desc".to_vec(), PdfString::text(kind));
        }
        let key = PdfString::text(&unique).bytes;
        entries.push((key, Object::Ref(doc.add(spec))));
    }
    entries.sort_by(|a, b| a.0.cmp(&b.0));
    let mut names = Vec::with_capacity(entries.len() * 2);
    for (k, v) in entries {
        names.push(Object::String(PdfString::literal(k)));
        names.push(v);
    }
    let mut tree = Dict::new();
    tree.set(b"Names".to_vec(), Object::Array(names));
    let tree = doc.add(tree);
    let root = doc.root().ok_or_else(|| CreateError::Invalid("the new document has no catalog".into()))?;
    doc.update_dict(root, |c| {
        let mut n = Dict::new();
        n.set(b"EmbeddedFiles".to_vec(), Object::Ref(tree));
        c.set(b"Names".to_vec(), Object::Dict(n));
    })
    .map_err(|e| CreateError::Invalid(format!("couldn't add the attachments: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = "From: Ada Lovelace <ada@example.org>\r\nTo: Charles <cb@example.org>, team@example.org\r\nSubject: =?UTF-8?Q?Engine_notes_=E2=80=94_draft?=\r\nDate: Sat, 11 Oct 2026 09:30:00 +1100\r\nMIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=\"b1\"\r\n\r\n--b1\r\nContent-Type: text/plain; charset=utf-8\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\nThe engine computes Bernoulli numbers.=0D=0ASecond line.\r\n--b1\r\nContent-Type: application/octet-stream; name=\"../../evil\\\\table.csv\"\r\nContent-Disposition: attachment; filename=\"../../evil\\\\table.csv\"\r\nContent-Transfer-Encoding: base64\r\n\r\nYSxiCjEsMgo=\r\n--b1--\r\n";

    #[test]
    fn plain_message_with_an_attachment() {
        let (doc, report) = from_email("note", PLAIN.as_bytes(), crate::LETTER, true).unwrap();
        assert_eq!(report.subject, "Engine notes \u{2014} draft");
        assert_eq!(report.attachments, vec!["table.csv".to_string()], "path parts are stripped");
        let catalog = doc.root().and_then(|r| doc.get(r).as_dict().cloned()).unwrap();
        let names = catalog.get(b"Names").and_then(Object::as_dict).cloned().unwrap();
        assert!(names.contains(b"EmbeddedFiles"));
        let (_, listed) = from_email("note", PLAIN.as_bytes(), crate::LETTER, false).unwrap();
        assert!(!listed.embedded);
    }

    #[test]
    fn html_only_message_becomes_text_without_fetching() {
        let html = "From: a@example.org\r\nSubject: News\r\nContent-Type: text/html; charset=utf-8\r\n\r\n<html><body><p>Hello <b>there</b></p><img src=\"https://tracker.example/pixel.gif\"><script>alert(1)</script></body></html>\r\n";
        let (doc, report) = from_email("n", html.as_bytes(), crate::LETTER, true).unwrap();
        assert_eq!(report.subject, "News");
        assert!(report.attachments.is_empty());
        assert_eq!(pdfcraft_model::pages(&doc).len(), 1);
    }

    #[test]
    fn not_an_email_and_odd_input() {
        assert!(from_email("x", b"", crate::LETTER, true).is_err());
        for odd in [&b"Subject: only a header"[..], b"\xff\xfe\x00junk", b"Content-Type: multipart/mixed; boundary=x\r\n\r\n--x\r\n--x\r\n--x--"] {
            let _ = from_email("x", odd, crate::LETTER, true);
        }
    }

    #[test]
    fn file_names_are_sanitised() {
        assert_eq!(safe_file_name(Some("../../etc/passwd"), "f"), "passwd");
        assert_eq!(safe_file_name(Some("C:\\Users\\x\\a.exe"), "f"), "a.exe");
        assert_eq!(safe_file_name(Some("..."), "f"), "f");
        assert_eq!(safe_file_name(Some("a\u{0}b\u{7}c"), "f"), "abc");
        assert_eq!(safe_file_name(None, "fallback"), "fallback");
        assert_eq!(safe_file_name(Some(&"n".repeat(500)), "f").chars().count(), 200);
    }
}
