//! Small string helpers for this crate and the crates built on it (the backend and the CLI). The
//! ingest path does not link biffix-workflow, so they are not for it.

/// `s` cut to at most `n` characters (not bytes), at a character boundary. The whole of `s` when
/// it is no longer than that.
pub fn truncate_chars(s: &str, n: usize) -> &str {
    match s.char_indices().nth(n) {
        Some((at, _)) => &s[..at],
        None => s,
    }
}

/// The last segment of a `/`-separated URI or path: a file's name.
pub fn basename(uri: &str) -> &str {
    uri.rsplit('/').next().unwrap_or(uri)
}

/// A URI's folder: `uri` up to and including its last `/` (empty when it has none), so
/// `parent(uri) + basename(uri) == uri`.
pub fn parent(uri: &str) -> &str {
    &uri[..uri.len() - basename(uri).len()]
}

/// Why a regex did not compile, on one line: the last line of the parser's multi-line report (which
/// draws carets under the pattern), without its `error: ` prefix.
pub(crate) fn regex_error_reason(e: &regex::Error) -> String {
    let text = e.to_string();
    let reason = text.lines().rev().map(str::trim).find(|l| !l.is_empty()).unwrap_or("invalid");
    reason.strip_prefix("error: ").unwrap_or(reason).to_string()
}

/// One CSV field as RFC 4180 writes it: quoted, its quotes doubled, when it holds a comma, a quote
/// or a line break; else as is. The one quoting rule of the workspace's CSV writers (the engine's
/// samplesheet, the backend's spreadsheet downloads).
pub fn csv_quote(v: &str) -> String {
    quote_field(v, ',')
}

/// One field of a `delimiter`-separated file as Python's `csv` writer writes it (minimal quoting,
/// the `excel` and `excel-tab` dialects): quoted, its quotes doubled, when it holds the delimiter, a
/// quote or a line break; else as is.
pub(crate) fn quote_field(v: &str, delimiter: char) -> String {
    if v.contains([delimiter, '"', '\n', '\r']) {
        format!("\"{}\"", v.replace('"', "\"\""))
    } else {
        v.to_string()
    }
}

/// Characters an address may legally hold that would break a `mailto:` link (a query, a fragment,
/// a percent escape, a second recipient) or an email header, so they are refused on purpose. Some
/// real addresses do use them (`=` and `%` in forwarding schemes such as SRS or VERP); such an
/// address is turned away, with [`EMAIL_RULE`] saying why.
const EMAIL_UNSAFE: [char; 8] = ['?', '&', '#', '%', ',', '<', '>', '='];

/// What [`plausible_email`] accepts, for the message that turns an address away.
pub const EMAIL_RULE: &str =
    "one `@`, a dotted domain, no spaces, and none of `? & # % , < > =` (unsafe in a mail link)";

/// A plausible single email address: one `@`, a non-empty local part, a dotted domain, no spaces,
/// none of `EMAIL_UNSAFE`, within the RFC length limit. Rejects typos and injection (into an SNS
/// subscription or a `mailto:` link); delivery is the real validation.
pub fn plausible_email(email: &str) -> bool {
    let Some((local, domain)) = email.split_once('@') else {
        return false;
    };
    email.len() <= 254
        && !local.is_empty()
        && !domain.contains('@')
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !email.chars().any(|c| c.is_whitespace() || c.is_control() || EMAIL_UNSAFE.contains(&c))
}

/// Whether `url` is an absolute `http://` or `https://` URL with a non-empty host and no spaces (a
/// link people are sent to, never a `javascript:` or `data:` one). The literal prefix and the space
/// check come first because `url::Url` is lenient: it would encode a space and read
/// `https:///path` as host `path`.
pub(crate) fn is_http_url(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    let rest = lower.strip_prefix("https://").or_else(|| lower.strip_prefix("http://"));
    rest.is_some_and(|r| !r.starts_with('/'))
        && !url.chars().any(|c| c.is_whitespace() || c.is_control())
        && url::Url::parse(url).is_ok_and(|u| u.host_str().is_some_and(|h| !h.is_empty()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_uri_splits_into_its_folder_and_its_name() {
        assert_eq!(basename("s3://b/x/y.zip"), "y.zip");
        assert_eq!(basename("nofolder"), "nofolder");
        assert_eq!(basename("s3://b/x/"), "");
        assert_eq!(parent("s3://b/x/y.zip"), "s3://b/x/");
        assert_eq!(parent("s3://b/y.zip"), "s3://b/");
        assert_eq!(parent("nofolder"), "");
        assert_eq!(parent("s3://b/x/"), "s3://b/x/");
    }

    #[test]
    fn a_csv_field_is_quoted_only_when_it_holds_a_delimiter_quote_or_line_break() {
        assert_eq!(csv_quote("plain"), "plain");
        assert_eq!(csv_quote("a,b"), "\"a,b\"");
        assert_eq!(csv_quote("say \"hi\""), "\"say \"\"hi\"\"\"");
        assert_eq!(csv_quote("two\r\nlines"), "\"two\r\nlines\"");
    }

    #[test]
    fn cuts_at_a_character_boundary() {
        assert_eq!(truncate_chars("héllo", 2), "hé");
        assert_eq!(truncate_chars("héllo", 5), "héllo");
        assert_eq!(truncate_chars("héllo", 9), "héllo");
        assert_eq!(truncate_chars("héllo", 0), "");
        assert_eq!(truncate_chars("", 3), "");
    }

    #[test]
    fn email_validation_rejects_typos_and_accepts_ordinary_addresses() {
        for ok in ["ops@example.org", "first.last+tag@sub.example.co.uk"] {
            assert!(plausible_email(ok), "{ok}");
        }
        for bad in [
            "",
            "ops",
            "ops@",
            "@example.org",
            "ops@example",
            "ops@@example.org",
            "ops@example.org.",
            "o ps@example.org",
            "ops@.example.org",
            "a?bcc=x@lab.example.org",
            "a@lab.example.org?bcc=x",
            "core@lab.example.org#x",
            "a&b@lab.example.org",
            "a%40b@lab.example.org",
            "a@lab.example.org,b@lab.example.org",
            "<a@lab.example.org>",
            "a=b@lab.example.org",
        ] {
            assert!(!plausible_email(bad), "{bad:?}");
        }
    }

    #[test]
    fn only_http_and_https_urls_with_a_non_empty_host_are_links() {
        for ok in [
            "https://lab.example.org/help",
            "HTTP://lab.example.org",
            "https://x.y/a?b=c#d",
            "http://[::1]:8080/x",
            "https://user@lab.example.org:8443/help",
        ] {
            assert!(is_http_url(ok), "{ok}");
        }
        for bad in [
            "",
            "lab.example.org/help",
            "javascript:alert(1)",
            "data:text/html,x",
            "ftp://lab.example.org",
            "https://",
            "https:///path",
            "https://lab.example.org/a b",
            "mailto:a@example.org",
            "https://?x",
            "https://#frag",
            "https://:80",
        ] {
            assert!(!is_http_url(bad), "{bad:?}");
        }
    }
}
