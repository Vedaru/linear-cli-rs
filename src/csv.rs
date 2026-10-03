//! The CSV half of `linear export` / `linear import`.
//!
//! Hand-rolled rather than pulled in as a dependency: the crate keeps its dependency list to what
//! the CLI is *about*, and the subset of RFC 4180 an export needs is small enough to pin with
//! tests - quote a field that contains a comma, a quote or a newline, escape a quote by doubling
//! it, and read the same grammar back. An issue description is markdown with commas, quotes and
//! blank lines in it, so the codec has to survive exactly that.
//!
//! The parser is deliberately strict about *shape* and quiet about meaning: a row with a different
//! number of cells than the header is the caller's problem to report, and unquoted whitespace is
//! kept as it was written.

use crate::errors::{CliError, Result};

/// One field, quoted only when it has to be.
pub fn encode_field(value: &str) -> String {
    let needs_quotes = value.contains([',', '"', '\n', '\r']);
    if !needs_quotes {
        return value.to_string();
    }
    format!("\"{}\"", value.replace('"', "\"\""))
}

/// One record, without its line ending.
pub fn encode_row(cells: &[String]) -> String {
    cells
        .iter()
        .map(|cell| encode_field(cell))
        .collect::<Vec<_>>()
        .join(",")
}

/// Parse a whole document into rows of cells.
///
/// A trailing newline does not produce a phantom empty record; an empty document is no rows at
/// all. Quoted fields may contain newlines, which is why this cannot be a `lines()` loop.
pub fn parse(text: &str) -> Result<Vec<Vec<String>>> {
    let mut rows: Vec<Vec<String>> = Vec::new();
    let mut row: Vec<String> = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = text.chars().peekable();
    let mut saw_any = false;

    while let Some(character) = chars.next() {
        saw_any = true;
        if quoted {
            match character {
                '"' => {
                    if chars.peek() == Some(&'"') {
                        chars.next();
                        cell.push('"');
                    } else {
                        quoted = false;
                    }
                }
                _ => cell.push(character),
            }
            continue;
        }

        match character {
            '"' if cell.is_empty() => quoted = true,
            ',' => {
                row.push(std::mem::take(&mut cell));
            }
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    chars.next();
                }
                push_record(&mut rows, &mut row, &mut cell);
            }
            '\n' => push_record(&mut rows, &mut row, &mut cell),
            _ => cell.push(character),
        }
    }

    if quoted {
        return Err(
            CliError::validation("The CSV ends inside a quoted field (an unclosed `\"`)")
                .suggestion(
                    "Close the quoted field, or export it again to see the shape it should have.",
                ),
        );
    }

    if !saw_any {
        return Ok(rows);
    }
    // The last record only counts if something was written after the previous newline.
    if !cell.is_empty() || !row.is_empty() {
        row.push(cell);
        rows.push(row);
    }

    Ok(rows)
}

/// End the current record, unless there is nothing in it.
///
/// A blank line is a line break, not a record with one empty field: CSV files end with a newline
/// and often carry blank lines, and either would otherwise arrive as a row that does not line up
/// with the header.
fn push_record(rows: &mut Vec<Vec<String>>, row: &mut Vec<String>, cell: &mut String) {
    if row.is_empty() && cell.is_empty() {
        return;
    }
    row.push(std::mem::take(cell));
    rows.push(std::mem::take(row));
}

/// A parsed document with its header row separated out.
pub struct Table {
    pub header: Vec<String>,
    pub rows: Vec<Vec<String>>,
}

impl Table {
    /// Split a document into header and rows, refusing the shapes an import cannot use.
    pub fn parse(text: &str) -> Result<Table> {
        let mut rows = parse(text)?;
        if rows.is_empty() {
            return Err(
                CliError::validation("The CSV has no header row").suggestion(
                    "Export a team first (`linear export issues --format csv`) to see the columns.",
                ),
            );
        }
        let header: Vec<String> = rows
            .remove(0)
            .into_iter()
            .map(|name| name.trim().to_string())
            .collect();

        for (index, row) in rows.iter().enumerate() {
            if row.len() != header.len() {
                return Err(CliError::validation(format!(
                    "CSV row {} has {} cells but the header has {}",
                    index + 2,
                    row.len(),
                    header.len()
                ))
                .suggestion(
                    "A row that does not line up with the header cannot be read as fields; fix the row or re-export it.",
                ));
            }
        }

        Ok(Table { header, rows })
    }

    /// The cell for a column, by name, or `None` when the column is absent.
    pub fn cell<'a>(&self, row: &'a [String], column: &str) -> Option<&'a str> {
        self.header
            .iter()
            .position(|name| name.eq_ignore_ascii_case(column))
            .and_then(|index| row.get(index))
            .map(String::as_str)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_field_is_not_quoted() {
        assert_eq!(encode_field("Build log"), "Build log");
        assert_eq!(encode_row(&["a".into(), "b".into()]), "a,b");
    }

    #[test]
    fn a_field_with_a_comma_a_quote_or_a_newline_is_quoted() {
        assert_eq!(encode_field("a,b"), "\"a,b\"");
        assert_eq!(encode_field("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(encode_field("line one\nline two"), "\"line one\nline two\"");
    }

    #[test]
    fn parsing_reads_back_what_encoding_wrote() {
        let cells = vec![
            "ENG-1".to_string(),
            "A title, with a comma".to_string(),
            "a \"quoted\" word".to_string(),
            "line one\nline two".to_string(),
            String::new(),
        ];
        let text = format!("{}\n", encode_row(&cells));
        let rows = parse(&text).expect("round trip");
        assert_eq!(rows, vec![cells]);
    }

    #[test]
    fn a_trailing_newline_is_not_a_phantom_row() {
        assert_eq!(parse("a,b\n").expect("parse").len(), 1);
        assert!(parse("").expect("parse").is_empty());
        assert_eq!(
            parse("a,b\n\n").expect("parse"),
            vec![vec!["a".to_string(), "b".to_string()]]
        );
    }

    #[test]
    fn crlf_is_accepted() {
        assert_eq!(
            parse("a,b\r\nc,d\r\n").expect("parse"),
            vec![
                vec!["a".to_string(), "b".to_string()],
                vec!["c".to_string(), "d".to_string()]
            ]
        );
    }

    #[test]
    fn an_unclosed_quote_is_refused() {
        assert!(parse("a,\"b\n").is_err());
    }

    #[test]
    fn a_row_that_does_not_line_up_with_the_header_is_refused() {
        assert!(Table::parse("a,b\n1\n").is_err());
        let table = Table::parse("a,b\n1,2\n").expect("two columns");
        assert_eq!(table.header, vec!["a".to_string(), "b".to_string()]);
        assert_eq!(table.cell(&table.rows[0], "B"), Some("2"));
        assert_eq!(table.cell(&table.rows[0], "missing"), None);
    }
}
