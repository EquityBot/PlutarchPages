//! External converters and Read Out Loud (PlutarchPages): Office files through an installed
//! LibreOffice, HTML files and web pages through an installed Chrome, Edge or Chromium, and page
//! text for the system's speech command. See `pdfcraft-external`.
//!
//! Conversion is off unless the user turns it on (Preferences ▸ External converters, or
//! `PDFCRAFT_EXTERNAL_CONVERTERS=1` for the command line and MCP server): it hands the file to
//! another program, outside PlutarchPages' own parsing and without an operating-system sandbox.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use pdfcraft_render::{PageRenderer, RenderConfig, RenderRequest, RequestKind};

#[cfg(not(target_arch = "wasm32"))]
use crate::open_source;
use crate::{DocId, EditError, Session, SourceKind, guard};

#[cfg(not(target_arch = "wasm32"))]
use pdfcraft_external::MAX_SPEECH;
/// The web build speaks nothing; gathering text stays bounded the same way.
#[cfg(target_arch = "wasm32")]
const MAX_SPEECH: usize = 1 << 20;

/// Turns the external converters on when set to `1`.
pub const ENABLE_VAR: &str = "PDFCRAFT_EXTERNAL_CONVERTERS";
/// The browser to use (an absolute path), instead of looking for one.
pub const BROWSER_VAR: &str = "PDFCRAFT_CHROME";
/// The LibreOffice `soffice` program to use (an absolute path), instead of looking for one.
pub const OFFICE_VAR: &str = "PDFCRAFT_SOFFICE";

/// The most pages Read Out Loud reads in one go.
pub const READ_PAGES: usize = 50;

/// Text gathered for Read Out Loud.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SpeechText {
    pub text: String,
    /// The pages (0-based) whose text is in `text`; the last may be in part.
    pub pages: Vec<usize>,
    /// Text was left out to stay within [`MAX_SPEECH`].
    pub truncated: bool,
}

/// Preferences ▸ External converters.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ExternalConverters {
    /// Off by default.
    pub enabled: bool,
    /// Chrome, Edge or Chromium; `None` looks in the usual places.
    pub browser: Option<PathBuf>,
    /// LibreOffice's `soffice`; `None` looks in the usual places.
    pub office: Option<PathBuf>,
}

#[cfg(not(target_arch = "wasm32"))]
fn off(name: &str, what: &str) -> EditError {
    EditError::Source(format!(
        "{name}: {what} can't be converted while External converters are off. They use programs installed on this computer \
         (LibreOffice, or Chrome or Edge); turn them on in Preferences > External converters, or set {ENABLE_VAR}=1 for the command line"
    ))
}

impl ExternalConverters {
    /// From the environment: [`ENABLE_VAR`] `=1` turns them on; [`BROWSER_VAR`] and [`OFFICE_VAR`]
    /// name the programs. The environment is the user's own configuration, never a document's.
    pub fn from_env() -> Self {
        let path = |v: &str| std::env::var_os(v).map(PathBuf::from).filter(|p| !p.as_os_str().is_empty());
        ExternalConverters { enabled: std::env::var(ENABLE_VAR).is_ok_and(|v| v.trim() == "1"), browser: path(BROWSER_VAR), office: path(OFFICE_VAR) }
    }

    /// Convert `bytes` (an HTML or Office file named `name`) to checked PDF bytes. `cancel` stops it.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn convert(&self, kind: SourceKind, name: &str, bytes: &[u8], cancel: &AtomicBool) -> Result<Vec<u8>, EditError> {
        use pdfcraft_external as ext;
        let what = match kind {
            SourceKind::Html => "HTML files",
            SourceKind::Office => "Office files",
            _ => return Err(EditError::Source(format!("{name}: not an HTML or Office file"))),
        };
        if !self.enabled {
            return Err(off(name, what));
        }
        let failed = |e: ext::ExternalError| EditError::Source(format!("{name}: {e}"));
        let limits = ext::Limits::default();
        let pdf = match kind {
            SourceKind::Html => ext::html_to_pdf(&ext::find_browser(self.browser.as_deref()).map_err(failed)?, name, bytes, &limits, cancel),
            _ => ext::office_to_pdf(&ext::find_office(self.office.as_deref()).map_err(failed)?, name, bytes, &limits, cancel),
        }
        .map_err(failed)?;
        checked(name, pdf)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn convert(&self, _kind: SourceKind, name: &str, _bytes: &[u8], _cancel: &AtomicBool) -> Result<Vec<u8>, EditError> {
        Err(EditError::Source(format!("{name}: converting Office and HTML files needs the desktop app")))
    }

    /// Print the web page at `url` (http or https; see `checked_web_url` for what the address
    /// check covers) to checked PDF bytes.
    #[cfg(not(target_arch = "wasm32"))]
    pub fn web_page(&self, url: &str, cancel: &AtomicBool) -> Result<Vec<u8>, EditError> {
        use pdfcraft_external as ext;
        let checked_url = ext::checked_web_url(url).map_err(|e| EditError::Source(e.to_string()))?;
        if !self.enabled {
            return Err(off(checked_url.as_str(), "web pages"));
        }
        let failed = |e: ext::ExternalError| EditError::Source(format!("{checked_url}: {e}"));
        let browser = ext::find_browser(self.browser.as_deref()).map_err(failed)?;
        let pdf = ext::url_to_pdf(&browser, checked_url.as_str(), &ext::Limits::default(), cancel).map_err(failed)?;
        checked(checked_url.as_str(), pdf)
    }

    #[cfg(target_arch = "wasm32")]
    pub fn web_page(&self, url: &str, _cancel: &AtomicBool) -> Result<Vec<u8>, EditError> {
        Err(EditError::Source(format!("{url}: converting web pages needs the desktop app")))
    }
}

/// A document name for the web page at `url` (its host, `example.org.html`), after checking the
/// address; the converted PDF is named after it.
pub fn web_page_name(url: &str) -> Result<String, EditError> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let u = pdfcraft_external::checked_web_url(url).map_err(|e| EditError::Source(e.to_string()))?;
        Ok(format!("{}.html", u.host_str().unwrap_or("web page")))
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = url;
        Ok("web page.html".into())
    }
}

/// The converter's output must open as a PDF here before anyone sees it (parsing untrusted bytes:
/// a panic is caught).
#[cfg(not(target_arch = "wasm32"))]
fn checked(name: &str, pdf: Vec<u8>) -> Result<Vec<u8>, EditError> {
    let bytes = Arc::new(pdf);
    guard(|| open_source(name, &bytes).map(|_| ()))
        .map_err(|_| EditError::Source(format!("{name}: the converted file could not be read")))?
        .map_err(|e| EditError::Source(format!("{name}: the converted file is not a usable PDF ({e})")))?;
    Ok(Arc::try_unwrap(bytes).unwrap_or_else(|b| (*b).clone()))
}

impl Session {
    /// The external converters in use (off unless set).
    pub fn external_converters(&self) -> &ExternalConverters {
        &self.external
    }

    pub fn set_external_converters(&mut self, converters: ExternalConverters) {
        self.external = converters;
    }

    /// Create ▸ From web page: the page at `url` as new, unsaved PDF bytes.
    pub fn create_from_web_page(&self, url: &str) -> Result<Arc<Vec<u8>>, EditError> {
        Ok(Arc::new(self.external.web_page(url, &AtomicBool::new(false))?))
    }

    /// The text of `pages` (0-based, at most [`READ_PAGES`]) for Read Out Loud, in order, stopping
    /// at [`MAX_SPEECH`] bytes. See [`Session::speech_text`].
    pub fn speech_pages(&self, id: DocId, pages: &[usize]) -> Result<SpeechText, EditError> {
        if pages.len() > READ_PAGES {
            return Err(EditError::Source(format!("Read Out Loud reads at most {READ_PAGES} pages at a time")));
        }
        let mut out = SpeechText::default();
        for &page in pages {
            let text = self.speech_text(id, page)?;
            out.pages.push(page);
            let room = MAX_SPEECH.saturating_sub(out.text.len());
            if text.len() >= room {
                let cut = text.floor_char_boundary(room);
                out.text.push_str(text.get(..cut).unwrap_or_default());
                out.truncated = cut < text.len() || out.pages.len() < pages.len();
                break;
            }
            out.text.push_str(&text);
            out.text.push('\n');
        }
        Ok(out)
    }

    /// The text of `page` (0-based) for Read Out Loud. Reading aloud is assistive use, which
    /// ISO 32000-2 asks readers to allow whatever the copy permission says (it deprecates the
    /// separate accessibility bit); this text goes only to the speech program, never to the
    /// clipboard, an export or a tool result.
    pub fn speech_text(&self, id: DocId, page: usize) -> Result<String, EditError> {
        let doc = self.get(id).ok_or(EditError::NoDocument)?;
        if page >= doc.info.pages.len() {
            return Err(EditError::Source(format!("there is no page {}", page.saturating_add(1))));
        }
        let config = RenderConfig { password: doc.password.as_deref().map(Arc::from), ..Default::default() };
        let bytes = doc.bytes.clone();
        let out = guard(move || {
            PageRenderer::new(bytes, config).render(RenderRequest { page, kind: RequestKind::Text, preview: false, tile: None, scale: 1.0, tag: 0 })
        })
        .map_err(|_| EditError::Source(format!("page {}: the text could not be read", page + 1)))?;
        match out.error {
            Some(e) => Err(EditError::Source(format!("page {}: {e}", page + 1))),
            None => Ok(out.text.map(|t| t.plain_text()).unwrap_or_default()),
        }
    }
}

#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use super::*;

    /// create and the external crate agree on which files go to which program.
    #[test]
    fn extension_lists_agree() {
        assert_eq!(pdfcraft_create::HTML_FILES.as_slice(), pdfcraft_external::HTML);
        assert_eq!(pdfcraft_create::OFFICE_FILES.as_slice(), pdfcraft_external::OFFICE);
    }

    #[test]
    fn converters_are_off_by_default_and_web_addresses_are_checked_first() {
        let s = Session::new();
        assert!(!s.external_converters().enabled);
        let err = s.create_from_web_page("http://127.0.0.1/admin").unwrap_err();
        assert!(matches!(&err, EditError::Source(m) if m.contains("private network")), "{err}");
        let err = s.create_from_web_page("https://example.org/").unwrap_err();
        assert!(matches!(&err, EditError::Source(m) if m.contains("External converters are off")), "{err}");
    }

    #[test]
    fn speech_text_ignores_the_copy_permission_but_not_the_page_range() {
        let mut doc = pdfcraft_cos::Document::open(Arc::new(crate::tests::fixture(1))).unwrap();
        doc.set_encryption(&pdfcraft_cos::NewEncryption {
            algorithm: pdfcraft_cos::Algorithm::Aes256,
            user_password: "",
            owner_password: "owner",
            permissions: 0b0100,
            encrypt_metadata: true,
            seed: [5; 32],
        })
        .unwrap();
        let bytes = Arc::new(pdfcraft_cos::write_full(&doc, &Default::default()).unwrap());
        let mut s = Session::new();
        let id = s.open("locked.pdf", None, bytes, None).unwrap();
        assert!(!s.get(id).unwrap().allows_copying());
        assert_eq!(s.speech_text(id, 0).unwrap().trim(), "Page 1", "assistive reading is allowed");
        assert!(s.speech_text(id, 5).is_err());
    }

    /// Final-review finding F6: at most 50 pages, gathered in order and reported.
    #[test]
    fn speech_pages_are_bounded() {
        let mut s = Session::new();
        let id = s.open("three.pdf", None, Arc::new(crate::tests::fixture(3)), None).unwrap();
        let read = s.speech_pages(id, &[2, 0]).unwrap();
        assert_eq!((read.pages, read.truncated), (vec![2, 0], false));
        assert!(read.text.find("Page 3") < read.text.find("Page 1"), "{}", read.text);
        assert!(s.speech_pages(id, &[0; READ_PAGES + 1]).is_err());
    }
}
