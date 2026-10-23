//! Path globs for a workflow's declared reports: `*` matches within one path
//! segment, `**` across segments, `?` one character. Also their SQL `LIKE` over-approximation, so a
//! store can narrow candidates before [`matches`](fn@matches) decides exactly.

/// Whether `path` (slash-separated, relative) matches `pattern`.
///
/// Memoized on (pattern position, path position), so a pattern with many `*`/`**` costs at most
/// O(len(pattern) × len(path)²) instead of backtracking exponentially: it runs once per output row
/// on the reports endpoint.
pub fn matches(pattern: &str, path: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = path.chars().collect();
    let mut memo = Memo::new(p.len(), s.len());
    glob_at(&p, &s, 0, 0, &mut memo)
}

/// Results of (pattern position, text position) already decided.
struct Memo {
    width: usize,
    seen: Vec<Option<bool>>,
}

impl Memo {
    fn new(pattern_len: usize, text_len: usize) -> Self {
        Self { width: text_len + 1, seen: vec![None; (pattern_len + 1) * (text_len + 1)] }
    }
    fn get(&self, pi: usize, si: usize) -> Option<bool> {
        self.seen[pi * self.width + si]
    }
    fn set(&mut self, pi: usize, si: usize, v: bool) -> bool {
        self.seen[pi * self.width + si] = Some(v);
        v
    }
}

fn glob_at(p: &[char], s: &[char], pi: usize, si: usize, memo: &mut Memo) -> bool {
    if let Some(known) = memo.get(pi, si) {
        return known;
    }
    let result = match p.get(pi) {
        None => si == s.len(),
        Some('*') if p.get(pi + 1) == Some(&'*') => {
            // `**/` also matches no directories at all.
            let rest = pi + 2;
            (p.get(rest) == Some(&'/') && glob_at(p, s, rest + 1, si, memo))
                || (si..=s.len()).any(|i| glob_at(p, s, rest, i, memo))
        }
        Some('*') => {
            let mut matched = false;
            for i in si..=s.len() {
                if glob_at(p, s, pi + 1, i, memo) {
                    matched = true;
                    break;
                }
                if s.get(i) == Some(&'/') {
                    break;
                }
            }
            matched
        }
        Some('?') => s.get(si).is_some_and(|c| *c != '/') && glob_at(p, s, pi + 1, si + 1, memo),
        Some(c) => s.get(si) == Some(c) && glob_at(p, s, pi + 1, si + 1, memo),
    };
    memo.set(pi, si, result)
}

/// A SQL `LIKE` pattern (escape character `\`) matching at least every path `pattern` matches,
/// after `prefix`: each run of `*` becomes `%`, `?` becomes `_`, and `%`, `_` and `\` are escaped.
/// A `**/` becomes a lone `%`, since it also matches no directories at all.
pub fn to_sql_like(prefix: &str, pattern: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + pattern.len());
    let escape = |c: char, out: &mut String| {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    };
    for c in prefix.chars() {
        escape(c, &mut out);
    }
    let mut chars = pattern.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '*' => {
                let mut run = 1;
                while chars.peek() == Some(&'*') {
                    chars.next();
                    run += 1;
                }
                if run >= 2 && chars.peek() == Some(&'/') {
                    chars.next();
                }
                out.push('%');
            }
            '?' => out.push('_'),
            c => escape(c, &mut out),
        }
    }
    out
}

/// Whether `text` matches the SQL `LIKE` `pattern` (escape `\`), as Postgres would. Memoized like
/// [`matches`](fn@matches). The in-memory store uses it, and the tests use it to check
/// [`to_sql_like`] against [`matches`](fn@matches).
pub fn like_matches(pattern: &str, text: &str) -> bool {
    fn at(p: &[char], s: &[char], pi: usize, si: usize, memo: &mut Memo) -> bool {
        if let Some(known) = memo.get(pi, si) {
            return known;
        }
        let result = match p.get(pi) {
            None => si == s.len(),
            Some('%') => (si..=s.len()).any(|i| at(p, s, pi + 1, i, memo)),
            Some('_') => si < s.len() && at(p, s, pi + 1, si + 1, memo),
            Some('\\') if pi + 1 < p.len() => {
                s.get(si) == Some(&p[pi + 1]) && at(p, s, pi + 2, si + 1, memo)
            }
            Some(c) => s.get(si) == Some(c) && at(p, s, pi + 1, si + 1, memo),
        };
        memo.set(pi, si, result)
    }
    let p: Vec<char> = pattern.chars().collect();
    let s: Vec<char> = text.chars().collect();
    let mut memo = Memo::new(p.len(), s.len());
    at(&p, &s, 0, 0, &mut memo)
}

/// Whether `pattern` is a usable report path: relative, no `..` or empty segments, not blank.
pub(crate) fn is_safe_relative(pattern: &str) -> bool {
    !pattern.trim().is_empty()
        && !pattern.starts_with('/')
        && !pattern.contains('\\')
        && pattern.split('/').all(|seg| !seg.is_empty() && seg != "." && seg != "..")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_star_stays_in_its_segment_and_a_double_star_crosses() {
        assert!(matches("multiqc/multiqc_report.html", "multiqc/multiqc_report.html"));
        assert!(matches("multiqc/*.html", "multiqc/multiqc_report.html"));
        assert!(!matches("*.html", "multiqc/multiqc_report.html"));
        assert!(matches("**/*.html", "multiqc/multiqc_report.html"));
        assert!(matches("**/*.html", "top.html"));
        assert!(matches("reports/**/summary.pdf", "reports/a/b/summary.pdf"));
        assert!(matches("reports/**/summary.pdf", "reports/summary.pdf"));
        assert!(matches("qc/sample_?.html", "qc/sample_1.html"));
        assert!(!matches("qc/sample_?.html", "qc/sample_12.html"));
        assert!(!matches("qc/*.html", "qc/a.htm"));
    }

    #[test]
    fn the_like_pattern_admits_every_glob_match() {
        let like = to_sql_like("s3://b/runs/r_1/", "multiqc/*_report.html");
        assert_eq!(like, "s3://b/runs/r\\_1/multiqc/%\\_report.html");
        for path in ["multiqc/multiqc_report.html", "multiqc/x_report.html"] {
            assert!(like_matches(&like, &format!("s3://b/runs/r_1/{path}")), "{path}");
        }
        assert!(!like_matches(&like, "s3://b/runs/rX1/multiqc/multiqc_report.html"));
        let deep = to_sql_like("p/", "**/*.pdf");
        assert_eq!(deep, "p/%%.pdf");
        for path in ["a/b.pdf", "b.pdf"] {
            assert!(like_matches(&deep, &format!("p/{path}")), "{path}");
        }
        // A MultiQC report at the run's own root is found by the `**/` search.
        let multiqc = to_sql_like("", "**/multiqc_report.html");
        assert!(like_matches(&multiqc, "multiqc_report.html"));
        assert!(like_matches(&multiqc, "qc/multiqc_report.html"));
    }

    #[test]
    fn many_wildcards_do_not_backtrack_exponentially() {
        let pattern = "**/**/**/**/**/**/**/**/**/**/*a*b*c*d*e*f*x";
        let path = "a/".repeat(40) + &"ab".repeat(20);
        let started = std::time::Instant::now();
        assert!(!matches(pattern, &path));
        assert!(!like_matches(&to_sql_like("", pattern), &path));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    proptest::proptest! {
        /// Whatever `matches` accepts, the `LIKE` prefilter admits: the store's narrowing never
        /// drops a declared report.
        #[test]
        fn the_like_prefilter_admits_every_match(
            pattern in proptest::collection::vec(
                proptest::sample::select(vec!["a", "b", "/", "*", "**", "?", "**/", "_"]),
                1..8,
            ),
            path in "[ab_/]{0,12}",
        ) {
            let pattern: String = pattern.concat();
            if matches(&pattern, &path) {
                let like = to_sql_like("s3://b/r_1/", &pattern);
                proptest::prop_assert!(
                    like_matches(&like, &format!("s3://b/r_1/{path}")),
                    "{pattern} matches {path} but {like} does not admit it"
                );
            }
        }
    }

    #[test]
    fn report_paths_must_stay_inside_the_run() {
        assert!(is_safe_relative("multiqc/multiqc_report.html"));
        assert!(is_safe_relative("**/*.html"));
        for bad in ["", " ", "/abs.html", "../x.html", "a/../b", "a//b", "./a", "a\\b"] {
            assert!(!is_safe_relative(bad), "{bad:?}");
        }
    }
}
