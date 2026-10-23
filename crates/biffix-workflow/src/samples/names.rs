//! Reading sequencing file names: the stem without its extension, the clickable parts of a name,
//! and the orderings and loose matches ids are compared by.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Endings stripped from a sequencing file's name before it is read (longest first).
pub(super) const SEQ_EXTENSIONS: [&str; 6] =
    [".fastq.gz", ".fq.gz", ".fastq", ".fq", ".bam", ".cram"];
/// Whether `s` ends in the ASCII `suffix`, ignoring ASCII case. It compares bytes, so it neither
/// allocates nor slices a `str` inside a multi-byte character.
pub(super) fn ends_with_ignore_case(s: &str, suffix: &str) -> bool {
    s.len() >= suffix.len()
        && s.as_bytes()[s.len() - suffix.len()..].eq_ignore_ascii_case(suffix.as_bytes())
}
/// A name without its sequencing extension (`P1_R1.fastq.gz` → `P1_R1`).
pub fn stem(name: &str) -> &str {
    SEQ_EXTENSIONS
        .iter()
        .find(|ext| ends_with_ignore_case(name, ext))
        .map(|ext| &name[..name.len() - ext.len()])
        .unwrap_or(name)
}

/// One part of a file name, for clicking the parts that make up an id.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Token {
    pub text: String,
    /// A separator (`_`, `-`, `.`) rather than a word.
    pub separator: bool,
}

/// Split a name's stem into words and the separators between them: `P07_S7_L001` → `P07`, `_`,
/// `S7`, `_`, `L001`.
pub fn tokenize(name: &str) -> Vec<Token> {
    let mut tokens: Vec<Token> = Vec::new();
    for c in stem(name).chars() {
        let separator = matches!(c, '_' | '-' | '.');
        match tokens.last_mut() {
            Some(last) if !separator && !last.separator => last.text.push(c),
            _ => tokens.push(Token { text: c.to_string(), separator }),
        }
    }
    tokens
}
/// Sort ids the way a person counts: `P2` before `P10`.
pub(crate) fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    fn chunks(s: &str) -> Vec<(bool, &str)> {
        let mut out: Vec<(bool, &str)> = Vec::new();
        let mut start = 0;
        let bytes = s.as_bytes();
        for i in 1..=bytes.len() {
            if i == bytes.len() || bytes[i].is_ascii_digit() != bytes[start].is_ascii_digit() {
                out.push((bytes[start].is_ascii_digit(), &s[start..i]));
                start = i;
            }
        }
        out
    }
    let (ca, cb) = (chunks(a), chunks(b));
    for (x, y) in ca.iter().zip(cb.iter()) {
        let ord = match (x, y) {
            ((true, n), (true, m)) => {
                let (n, m) = (n.trim_start_matches('0'), m.trim_start_matches('0'));
                n.len().cmp(&m.len()).then_with(|| n.cmp(m))
            }
            ((_, s), (_, t)) => s.to_lowercase().cmp(&t.to_lowercase()),
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ca.len().cmp(&cb.len()).then_with(|| a.cmp(b))
}

/// An id with case and leading zeros ignored: `p007` and `P7` match.
pub(super) fn loose(id: &str) -> String {
    let mut out = String::new();
    let mut in_digits = false;
    let mut pending_zeros = 0;
    for c in id.to_lowercase().chars() {
        if c.is_ascii_digit() {
            if !in_digits && c == '0' {
                pending_zeros += 1;
                continue;
            }
            in_digits = true;
            out.push(c);
        } else {
            if !in_digits && pending_zeros > 0 {
                out.push('0');
            }
            in_digits = false;
            pending_zeros = 0;
            out.push(c);
        }
    }
    if !in_digits && pending_zeros > 0 {
        out.push('0');
    }
    out
}
