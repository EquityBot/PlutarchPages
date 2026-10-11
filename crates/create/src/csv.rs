//! CSV and tab-separated files to a PDF table (RFC 4180 quoting; the delimiter is a tab for
//! `.tsv`, otherwise sniffed from the first line among `,` `;` tab and `|`).

use pdfcraft_cos::Document;

use crate::CreateError;
use crate::flow::{Block, Face, MAX_COLUMNS, PageSetup, Para, Span, Table, render};

/// The most rows read; a longer file is cut and says so on its last page.
pub const MAX_ROWS: usize = 100_000;
/// The longest cell kept, in characters.
const MAX_CELL: usize = 10_000;

/// How a CSV file was read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CsvReport {
    pub rows: usize,
    pub columns: usize,
    /// Rows past [`MAX_ROWS`] that were left out.
    pub rows_dropped: usize,
    /// Columns past the layout's limit that were left out.
    pub columns_dropped: usize,
    /// Characters the standard fonts can't show, written as `?`.
    pub replaced_characters: usize,
}

/// The delimiter of a file named `name` whose text is `text`.
pub fn delimiter(name: &str, text: &str) -> char {
    if name.to_ascii_lowercase().ends_with(".tsv") {
        return '\t';
    }
    let first = first_record(text);
    [',', ';', '\t', '|'].into_iter().max_by_key(|d| count_outside_quotes(first, *d)).filter(|d| count_outside_quotes(first, *d) > 0).unwrap_or(',')
}

/// The first record's text (up to its first line break outside quotes), at most 64 KiB.
fn first_record(text: &str) -> &str {
    let mut quoted = false;
    for (i, c) in text.char_indices() {
        if i > 65_536 {
            return text.get(..i).unwrap_or(text);
        }
        match c {
            '"' => quoted = !quoted,
            '\n' | '\r' if !quoted => return text.get(..i).unwrap_or(text),
            _ => {}
        }
    }
    text
}

fn count_outside_quotes(s: &str, d: char) -> usize {
    let mut quoted = false;
    let mut n = 0;
    for c in s.chars() {
        if c == '"' {
            quoted = !quoted;
        } else if c == d && !quoted {
            n += 1;
        }
    }
    n
}

fn push_char(field: &mut String, n: &mut usize, c: char) {
    if *n < MAX_CELL {
        field.push(c);
    }
    *n = n.saturating_add(1);
}

fn end_row(row: &mut Vec<String>, rows: &mut Vec<Vec<String>>, dropped: &mut usize, max_rows: usize) {
    let r = std::mem::take(row);
    if r.len() == 1 && r.first().is_some_and(String::is_empty) {
        return; // a blank line
    }
    if rows.len() < max_rows {
        rows.push(r);
    } else {
        *dropped = dropped.saturating_add(1);
    }
}

/// Parse delimited text into rows of cells (RFC 4180: quoted fields may hold delimiters, line
/// breaks and doubled quotes; CRLF, LF and CR all end a record). Returns the rows and how many
/// were dropped past `max_rows`.
pub fn parse(text: &str, delim: char, max_rows: usize) -> (Vec<Vec<String>>, usize) {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut dropped = 0usize;
    let mut row: Vec<String> = Vec::new();
    let mut field = String::new();
    let mut field_chars = 0usize;
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if quoted {
            if c == '"' {
                if chars.peek() == Some(&'"') {
                    chars.next();
                    push_char(&mut field, &mut field_chars, '"');
                } else {
                    quoted = false;
                }
            } else {
                push_char(&mut field, &mut field_chars, c);
            }
            continue;
        }
        match c {
            '"' if field.is_empty() => quoted = true,
            '\r' | '\n' => {
                if c == '\r' && chars.peek() == Some(&'\n') {
                    chars.next();
                }
                row.push(std::mem::take(&mut field));
                field_chars = 0;
                end_row(&mut row, &mut rows, &mut dropped, max_rows);
            }
            _ if c == delim => {
                row.push(std::mem::take(&mut field));
                field_chars = 0;
            }
            _ => push_char(&mut field, &mut field_chars, c),
        }
    }
    if !field.is_empty() || !row.is_empty() {
        row.push(field);
        end_row(&mut row, &mut rows, &mut dropped, max_rows);
    }
    (rows, dropped)
}

/// A PDF with the rows of `text` as a table, the first row as a repeated header. Wide tables are
/// set on landscape pages in a smaller size.
pub fn from_csv(title: &str, name: &str, text: &str, page: (f64, f64)) -> Result<(Document, CsvReport), CreateError> {
    let delim = delimiter(name, text);
    let (mut rows, rows_dropped) = parse(text, delim, MAX_ROWS);
    if rows.is_empty() {
        return Err(CreateError::Invalid(format!("{name}: the file has no rows")));
    }
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    let columns_dropped = columns.saturating_sub(MAX_COLUMNS);
    for r in &mut rows {
        r.truncate(MAX_COLUMNS);
    }
    let shown = columns.min(MAX_COLUMNS);
    // Up to 6 columns fit portrait at 9 pt; wider tables turn the page and shrink the type.
    let (setup, size) = match shown {
        0..=6 => (PageSetup::portrait(page), 9.0),
        7..=10 => (PageSetup::landscape(page), 8.0),
        11..=16 => (PageSetup::landscape(page), 7.0),
        _ => (PageSetup::landscape(page), 6.0),
    };
    let mut report = CsvReport { rows: rows.len(), columns: shown, rows_dropped, columns_dropped, replaced_characters: 0 };
    let mut blocks =
        vec![Block::Para(Para { before: 0.0, after: 8.0, keep_with_next: true, ..Para::body(vec![Span::new(title, Face::Bold)], 13.0) })];
    blocks.push(Block::Table(Table { rows, header: true, size }));
    if rows_dropped > 0 || columns_dropped > 0 {
        let note = format!(
            "Cut to fit: {rows_dropped} more row(s) after the first {MAX_ROWS} and {columns_dropped} more column(s) after the first {MAX_COLUMNS} were left out."
        );
        blocks.push(Block::Para(Para::body(vec![Span::new(note, Face::Italic)], 8.0)));
    }
    let (doc, replaced) = render(title, &blocks, setup)?;
    report.replaced_characters = replaced;
    Ok((doc, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_delimiters_and_line_breaks() {
        let (rows, dropped) = parse("a,\"b, with comma\",c\r\n1,\"two\nlines\",\"say \"\"hi\"\"\"\n\nlast", ',', 10);
        assert_eq!(dropped, 0);
        assert_eq!(rows, vec![vec!["a", "b, with comma", "c"], vec!["1", "two\nlines", "say \"hi\""], vec!["last"]]);
    }

    #[test]
    fn sniffs_the_delimiter() {
        assert_eq!(delimiter("a.csv", "x;y;z\n1;2;3"), ';');
        assert_eq!(delimiter("a.csv", "\"a,b\";c\n"), ';', "commas inside quotes don't count");
        assert_eq!(delimiter("a.tsv", "x,y"), '\t');
        assert_eq!(delimiter("a.csv", "single"), ',');
    }

    #[test]
    fn rows_and_cells_are_capped() {
        let text = "a,b\n".repeat(50);
        let (rows, dropped) = parse(&text, ',', 10);
        assert_eq!((rows.len(), dropped), (10, 40));
        let (rows, _) = parse(&"x".repeat(MAX_CELL * 2), ',', 10);
        assert_eq!(rows.first().and_then(|r| r.first()).map(|c| c.chars().count()), Some(MAX_CELL));
    }

    #[test]
    fn unterminated_quote_and_odd_input_do_not_panic() {
        for s in ["\"open", "\"", ",,,\n,,", "\r\r\n\n", "a,\"b\"c,d", "\u{feff}h1,h2\n1,2"] {
            let _ = parse(s, ',', 100);
        }
        assert_eq!(parse("\u{feff}h1,h2", ',', 10).0, vec![vec!["h1", "h2"]]);
    }

    #[test]
    fn makes_a_table_document() {
        let csv: String =
            std::iter::once("Name,Amount,Note\n".to_string()).chain((0..300).map(|i| format!("Item {i},{}.50,\"note, {i}\"\n", i * 3))).collect();
        let (doc, report) = from_csv("Ledger", "ledger.csv", &csv, crate::LETTER).unwrap();
        assert_eq!(report, CsvReport { rows: 301, columns: 3, rows_dropped: 0, columns_dropped: 0, replaced_characters: 0 });
        assert!(pdfcraft_model::pages(&doc).len() > 1, "300 rows span pages");
        let wide: String = (0..80).map(|i| format!("c{i}")).collect::<Vec<_>>().join(",");
        let (_, report) = from_csv("Wide", "wide.csv", &wide, crate::LETTER).unwrap();
        assert_eq!((report.columns, report.columns_dropped), (MAX_COLUMNS, 80 - MAX_COLUMNS));
        assert!(from_csv("Empty", "e.csv", "\n\n", crate::LETTER).is_err());
    }
}
