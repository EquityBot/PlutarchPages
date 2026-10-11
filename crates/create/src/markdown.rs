//! Markdown (CommonMark with tables, strikethrough and task lists) to PDF. Headings, emphasis,
//! inline code, code blocks, block quotes, nested lists, rules, tables and links (as link
//! annotations) are laid out; images are never fetched: their alternative text is shown instead.
//! Raw HTML is left out.

use pdfcraft_cos::Document;
use pulldown_cmark::{Event, HeadingLevel, Options, Parser, Tag, TagEnd};

use crate::CreateError;
use crate::flow::{Block, Face, MAX_COLUMNS, PageSetup, Para, Span, Table, render};

const BODY: f64 = 10.5;
const LIST_INDENT: f64 = 18.0;
const QUOTE_INDENT: f64 = 14.0;
/// Deeper nesting than this is laid out at this depth.
const MAX_DEPTH: usize = 8;

#[derive(Default)]
struct State {
    blocks: Vec<Block>,
    spans: Vec<Span>,
    bold: usize,
    italic: usize,
    links: Vec<Option<String>>,
    /// The next number of each open list (`None` for bullets).
    lists: Vec<Option<u64>>,
    /// The marker of the list item whose first paragraph is still to come.
    marker: Option<String>,
    quotes: usize,
    heading: Option<HeadingLevel>,
    code: Option<String>,
    image_alt: Option<String>,
    table: Option<(Vec<Vec<String>>, Vec<String>, String)>,
}

impl State {
    fn indent(&self) -> f64 {
        self.lists.len().min(MAX_DEPTH) as f64 * LIST_INDENT + self.quotes.min(MAX_DEPTH) as f64 * QUOTE_INDENT
    }

    fn face(&self) -> Face {
        Face::styled(self.bold > 0 || self.heading.is_some(), self.italic > 0 || self.quotes > 0, false)
    }

    fn text(&mut self, t: &str, face: Face) {
        if let Some(alt) = self.image_alt.as_mut() {
            alt.push_str(t);
            return;
        }
        if let Some((_, _, cell)) = self.table.as_mut() {
            cell.push_str(t);
            return;
        }
        let link = self.links.last().cloned().flatten();
        match self.spans.last_mut() {
            Some(s) if s.face == face && s.link == link => s.text.push_str(t),
            _ => self.spans.push(Span { text: t.to_string(), face, link }),
        }
    }

    /// End the current paragraph-like block.
    fn flush(&mut self, size: f64, before: f64, keep_with_next: bool) {
        if self.spans.iter().all(|s| s.text.trim().is_empty()) {
            self.spans.clear();
            return;
        }
        let spans = std::mem::take(&mut self.spans);
        let marker = self.marker.take();
        let in_list = !self.lists.is_empty();
        self.blocks.push(Block::Para(Para {
            spans,
            size,
            indent: self.indent(),
            marker,
            before,
            after: if in_list { size * 0.25 } else { size * 0.6 },
            keep_with_next,
        }));
    }
}

fn heading_size(level: HeadingLevel) -> f64 {
    match level {
        HeadingLevel::H1 => 20.0,
        HeadingLevel::H2 => 16.0,
        HeadingLevel::H3 => 13.5,
        HeadingLevel::H4 => 12.0,
        HeadingLevel::H5 | HeadingLevel::H6 => BODY,
    }
}

/// The blocks of a Markdown document.
fn blocks(text: &str) -> Vec<Block> {
    let options = Options::ENABLE_TABLES | Options::ENABLE_STRIKETHROUGH | Options::ENABLE_TASKLISTS;
    let mut st = State::default();
    for event in Parser::new_ext(text, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { level, .. } => {
                    st.flush(BODY, 0.0, false);
                    st.heading = Some(level);
                }
                Tag::BlockQuote(_) => {
                    st.flush(BODY, 0.0, false);
                    st.quotes = st.quotes.saturating_add(1);
                }
                Tag::CodeBlock(_) => {
                    st.flush(BODY, 0.0, false);
                    st.code = Some(String::new());
                }
                Tag::List(start) => {
                    st.flush(BODY, 0.0, false);
                    st.lists.push(start);
                }
                Tag::Item => {
                    st.flush(BODY, 0.0, false);
                    let marker = match st.lists.last_mut() {
                        Some(Some(n)) => {
                            let m = format!("{n}.");
                            *n = n.saturating_add(1);
                            m
                        }
                        _ => "\u{2022}".to_string(),
                    };
                    st.marker = Some(marker);
                }
                Tag::Emphasis => st.italic = st.italic.saturating_add(1),
                Tag::Strong => st.bold = st.bold.saturating_add(1),
                Tag::Link { dest_url, .. } => st.links.push(Some(dest_url.into_string())),
                Tag::Image { .. } => st.image_alt = Some(String::new()),
                Tag::Table(_) => {
                    st.flush(BODY, 0.0, false);
                    st.table = Some((Vec::new(), Vec::new(), String::new()));
                }
                Tag::TableHead | Tag::TableRow => {
                    if let Some((_, row, _)) = st.table.as_mut() {
                        row.clear();
                    }
                }
                Tag::TableCell => {
                    if let Some((_, _, cell)) = st.table.as_mut() {
                        cell.clear();
                    }
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => st.flush(BODY, 0.0, false),
                TagEnd::Heading(level) => {
                    st.heading = None;
                    let size = heading_size(level);
                    let before = if st.blocks.is_empty() { 0.0 } else { size * 0.8 };
                    st.flush(size, before, true);
                }
                TagEnd::BlockQuote(_) => {
                    st.flush(BODY, 0.0, false);
                    st.quotes = st.quotes.saturating_sub(1);
                }
                TagEnd::CodeBlock => {
                    let code = st.code.take().unwrap_or_default();
                    let lines = code.strip_suffix('\n').unwrap_or(&code).split('\n').map(str::to_string).collect();
                    st.blocks.push(Block::Code { lines, size: 9.0, indent: st.indent() });
                }
                TagEnd::List(_) => {
                    st.flush(BODY, 0.0, false);
                    st.lists.pop();
                    if st.lists.is_empty() {
                        st.blocks.push(Block::Space(BODY * 0.4));
                    }
                }
                TagEnd::Item => st.flush(BODY, 0.0, false),
                TagEnd::Emphasis => st.italic = st.italic.saturating_sub(1),
                TagEnd::Strong => st.bold = st.bold.saturating_sub(1),
                TagEnd::Link => {
                    st.links.pop();
                }
                TagEnd::Image => {
                    let alt = st.image_alt.take().unwrap_or_default();
                    let shown = if alt.trim().is_empty() { "[image]".to_string() } else { format!("[image: {}]", alt.trim()) };
                    st.text(&shown, Face::styled(false, true, false));
                }
                TagEnd::TableCell => {
                    if let Some((_, row, cell)) = st.table.as_mut() {
                        row.push(std::mem::take(cell));
                    }
                }
                TagEnd::TableHead | TagEnd::TableRow => {
                    if let Some((rows, row, _)) = st.table.as_mut() {
                        let mut r = std::mem::take(row);
                        r.truncate(MAX_COLUMNS);
                        rows.push(r);
                    }
                }
                TagEnd::Table => {
                    if let Some((rows, ..)) = st.table.take() {
                        st.blocks.push(Block::Table(Table { rows, header: true, size: 9.0 }));
                    }
                }
                _ => {}
            },
            Event::Text(t) => {
                if let Some(code) = st.code.as_mut() {
                    code.push_str(&t);
                } else {
                    let face = st.face();
                    st.text(&t, face);
                }
            }
            Event::Code(t) => {
                let face = Face::styled(st.bold > 0 || st.heading.is_some(), false, true);
                st.text(&t, face);
            }
            Event::SoftBreak => {
                let face = st.face();
                st.text(" ", face);
            }
            Event::HardBreak => {
                let face = st.face();
                st.text("\n", face);
            }
            Event::Rule => {
                st.flush(BODY, 0.0, false);
                st.blocks.push(Block::Rule);
            }
            Event::TaskListMarker(done) => {
                let face = st.face();
                st.text(if done { "[x] " } else { "[ ] " }, face);
            }
            _ => {}
        }
    }
    st.flush(BODY, 0.0, false);
    st.blocks
}

/// A PDF from Markdown `text`, titled `title`, on pages of `page` points, and how many characters
/// the standard fonts could not show (written as `?`).
pub fn from_markdown(title: &str, text: &str, page: (f64, f64)) -> Result<(Document, usize), CreateError> {
    render(title, &blocks(text), PageSetup::portrait(page))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paras(b: &[Block]) -> Vec<String> {
        b.iter()
            .filter_map(|b| match b {
                Block::Para(p) => Some(p.spans.iter().map(|s| s.text.as_str()).collect()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn headings_emphasis_lists_and_links() {
        let b = blocks(
            "# Title\n\nSome *em* and **strong** and `code` with a [link](https://example.org).\n\n- one\n- two\n  1. nested\n\n---\n\n> quoted\n",
        );
        let p = paras(&b);
        assert_eq!(p.first().map(String::as_str), Some("Title"));
        assert!(p.contains(&"Some em and strong and code with a link.".to_string()), "{p:?}");
        assert!(b.iter().any(|b| matches!(b, Block::Rule)));
        let items: Vec<&Para> = b
            .iter()
            .filter_map(|b| match b {
                Block::Para(p) if p.marker.is_some() => Some(p),
                _ => None,
            })
            .collect();
        assert_eq!(items.iter().map(|p| p.marker.clone().unwrap_or_default()).collect::<Vec<_>>(), vec!["\u{2022}", "\u{2022}", "1."]);
        assert!(items.last().is_some_and(|p| p.indent > LIST_INDENT), "nested items indent further");
        let link = b.iter().find_map(|b| match b {
            Block::Para(p) => p.spans.iter().find(|s| s.link.is_some()).cloned(),
            _ => None,
        });
        assert_eq!(link.and_then(|s| s.link), Some("https://example.org".into()));
    }

    #[test]
    fn code_blocks_tables_and_images() {
        let b = blocks("```\nfn main() {}\n\tx\n```\n\n| a | b |\n|---|---|\n| 1 | 2 |\n\n![a chart](https://example.org/x.png)\n");
        assert!(b.iter().any(|b| matches!(b, Block::Code { lines, .. } if lines == &vec!["fn main() {}".to_string(), "\tx".to_string()])));
        assert!(b.iter().any(|b| matches!(b, Block::Table(t) if t.rows == vec![vec!["a".to_string(), "b".into()], vec!["1".into(), "2".into()]])));
        assert!(paras(&b).contains(&"[image: a chart]".to_string()), "images are not fetched; the alt text shows");
    }

    #[test]
    fn deep_nesting_and_odd_input_do_not_panic() {
        let deep = format!("{}x", "> ".repeat(500));
        let list = (0..300).map(|i| format!("{}- item", "  ".repeat(i))).collect::<Vec<_>>().join("\n");
        for s in [deep.as_str(), list.as_str(), "", "|", "<script>alert(1)</script>", "[x](javascript:alert(1))", "***\n___"] {
            let (doc, _) = from_markdown("t", s, crate::LETTER).unwrap();
            assert!(!pdfcraft_model::pages(&doc).is_empty());
        }
    }
}
