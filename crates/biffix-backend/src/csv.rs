//! CSV for people who open it in a spreadsheet: RFC 4180 records (CRLF line ends, a field quoted
//! when it holds a comma, a quote or a line break, its quotes doubled) whose text cells cannot run
//! as formulas.

/// The UTF-8 byte-order mark. Excel reads a CSV without one in the system's legacy code page, so a
/// sample named `Café` would show garbled; with it, Excel (and every CSV reader that knows UTF-8)
/// reads UTF-8.
pub(crate) const UTF8_BOM: &str = "\u{feff}";

/// The characters that start a formula in a spreadsheet when a cell begins with one (after any
/// leading whitespace, which some importers trim): `=`, `+`, `-`, `@` and their full-width forms
/// (which Excel in CJK locales also reads as formula starts).
const FORMULA_STARTS: [char; 8] = ['=', '+', '-', '@', '＝', '＋', '－', '＠'];

/// Whitespace a spreadsheet importer may trim before it reads a cell.
const LEADING_WHITESPACE: [char; 4] = [' ', '\t', '\r', '\n'];

/// Whether `raw` gets a leading `'` so a spreadsheet shows it as text (CSV injection, after OWASP's
/// CSV Injection guidance). It does when it starts with a tab, carriage return or line feed; when
/// its first character after leading spaces, tabs and line breaks is a formula start
/// ([`FORMULA_STARTS`]); and when it already starts with `'`. The last keeps the guard reversible:
/// every cell that starts with `'` had exactly one added, so a program reading the file (pandas, R)
/// gets the original value back by removing one leading `'` from every cell that has one.
///
/// Numbers are not exempt: `-5` is written `'-5`. A plain number cannot run, but OWASP's rule has no
/// such exception, and deciding what counts as one (`-2+3`? `-1e5`?) is parsing a guard should not
/// risk getting wrong; the columns it covers (sample ids, names, labels) are text anyway.
fn needs_guard(raw: &str) -> bool {
    raw.starts_with(['\t', '\r', '\n', '\''])
        || raw.trim_start_matches(LEADING_WHITESPACE).starts_with(FORMULA_STARTS)
}

/// One field: guarded against running as a formula (see [`needs_guard`]; the values are names
/// people choose), then RFC 4180 quoting.
pub(crate) fn field(raw: &str) -> String {
    if needs_guard(raw) {
        biffix_workflow::text::csv_quote(&format!("'{raw}"))
    } else {
        biffix_workflow::text::csv_quote(raw)
    }
}

/// One record: the fields (each through [`field`]) joined by commas, ended by CRLF.
pub(crate) fn record<S: AsRef<str>>(fields: &[S]) -> String {
    let mut line = fields.iter().map(|f| field(f.as_ref())).collect::<Vec<_>>().join(",");
    line.push_str("\r\n");
    line
}

/// How many bytes `text` takes as a JSON string's contents (without the quotes), the way
/// `serde_json` escapes it: `"` and `\` take two, a control character two (`\b \t \n \f \r`) or six
/// (`\u00XX`), every other byte one (non-ASCII is not escaped). The API Lambda answers a text body
/// inside a JSON envelope (`lambda_http` sends `text/*` as a JSON string), so this, not the CSV's own
/// length, is what counts against the Lambda's response-size limit.
pub(crate) fn json_escaped_len(text: &str) -> usize {
    text.bytes()
        .map(|b| match b {
            b'"' | b'\\' | 0x08 | b'\t' | b'\n' | 0x0c | b'\r' => 2,
            0..=0x1f => 6,
            _ => 1,
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_plain_field_is_as_is() {
        assert_eq!(field("S1"), "S1");
        assert_eq!(field(""), "");
        assert_eq!(field("Café 鼠"), "Café 鼠");
        assert_eq!(field("5"), "5");
    }

    #[test]
    fn a_field_with_a_separator_quote_or_line_break_is_quoted() {
        assert_eq!(field("Doe, Jane"), "\"Doe, Jane\"");
        assert_eq!(field("a\"b"), "\"a\"\"b\"");
        assert_eq!(field("two\nlines"), "\"two\nlines\"");
        assert_eq!(field("cr\rhere"), "\"cr\rhere\"");
    }

    #[test]
    fn a_field_that_would_run_as_a_formula_is_made_text() {
        for (raw, want) in [
            ("=1+1", "'=1+1"),
            ("+1", "'+1"),
            ("-1", "'-1"),
            ("-0.2", "'-0.2"),
            ("@SUM(A1)", "'@SUM(A1)"),
            ("\tx", "'\tx"),
            ("\rx", "\"'\rx\""),
            ("\nx", "\"'\nx\""),
            // After leading whitespace, which some importers trim.
            (" =1+1", "' =1+1"),
            ("  \t-ctrl", "'  \t-ctrl"),
            ("\n=1+1", "\"'\n=1+1\""),
            // Full-width formula starts.
            ("＝1+1", "'＝1+1"),
            ("＋1", "'＋1"),
            ("－1", "'－1"),
            ("＠x", "'＠x"),
            ("=HYPERLINK(\"x\",\"y\")", "\"'=HYPERLINK(\"\"x\"\",\"\"y\"\")\""),
        ] {
            assert_eq!(field(raw), want, "{raw:?}");
        }
        // Only a leading one counts, and whitespace alone is not a formula.
        assert_eq!(field("a=b-c"), "a=b-c");
        assert_eq!(field(" a"), " a");
    }

    #[test]
    fn a_field_that_starts_with_a_quote_gets_one_more_so_the_guard_can_be_undone() {
        assert_eq!(field("'quoted"), "''quoted");
        // Removing one leading `'` from every cell that has one gives each value back.
        for raw in ["'quoted", "=1", "-5", "plain", "''two"] {
            let cell = field(raw);
            assert_eq!(cell.strip_prefix('\'').unwrap_or(&cell), raw);
        }
    }

    #[test]
    fn a_record_joins_its_fields_and_ends_with_crlf() {
        assert_eq!(record(&["a", "b,c", ""]), "a,\"b,c\",\r\n");
        assert_eq!(record::<&str>(&[]), "\r\n");
    }

    #[test]
    fn the_json_escaped_length_matches_serde_json() {
        for text in [
            "",
            "plain",
            "a,\"b\"\r\n",
            "back\\slash",
            "\u{1}\u{1f}\u{8}\u{c}\t",
            "Café 鼠 \u{feff}",
            "\u{7f}",
        ] {
            let json = serde_json::to_string(text).unwrap();
            assert_eq!(json_escaped_len(text), json.len() - 2, "{text:?}");
        }
    }
}
