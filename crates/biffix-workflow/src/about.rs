//! A workflow's About page: the one-line summary shown where workflows are listed.

use regex::Regex;
use std::sync::OnceLock;

/// The longest summary, in characters; a longer first sentence is cut with an ellipsis.
pub(crate) const MAX_SUMMARY_CHARS: usize = 200;

/// A one-line, plain-text summary of a version: the form's description when the author wrote one,
/// else the first sentence of its About page. `None` when it has neither.
pub fn summary(form_description: Option<&str>, about_markdown: Option<&str>) -> Option<String> {
    let text = match form_description.map(str::trim).filter(|d| !d.is_empty()) {
        Some(description) => plain_text(description),
        None => {
            let sentence = first_sentence(&plain_text(&first_paragraph(about_markdown?)?));
            // A lead into a list (`Choose one of:`) says nothing on its own, as for a help.
            if sentence.trim_end().ends_with(':') {
                return None;
            }
            sentence
        }
    };
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    (!text.is_empty()).then(|| cap(&text))
}

/// A parameter's one-line, plain-text description from its help (Markdown, as the launch form's
/// help panel renders it): the first sentence ([`first_sentence`], so `e.g.`/`i.e.`/`vs.` do not
/// end it) of its first paragraph, with whitespace collapsed (a hard-wrapped `help: |` block reads
/// as one line) and Markdown reduced to text (a leading heading, list or quote marker dropped;
/// links, emphasis and code to their text). A paragraph ends at a blank line, after a heading, or
/// where a list starts,
/// so a help that leads into a list (`Choose one of:` then `- star`) gives no description rather
/// than a dangling one: `None` when what is left is empty or ends in `:`. Capped at
/// [`MAX_SUMMARY_CHARS`].
pub(crate) fn help_description(help: &str) -> Option<String> {
    let marker = block_marker();
    let mut lines: Vec<String> = Vec::new();
    for line in help.lines() {
        let t = line.trim();
        if t.is_empty() {
            if lines.is_empty() {
                continue;
            }
            break;
        }
        // A list (or a heading, a quote) after the first line starts a new block.
        if !lines.is_empty() && marker.is_match(t) {
            break;
        }
        lines.push(marker.replace(t, "").into_owned());
        // A heading is a block of its own.
        if t.starts_with('#') {
            break;
        }
    }
    let paragraph = plain_text(&lines.join(" "));
    let paragraph = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
    let sentence = first_sentence(&paragraph);
    let sentence = sentence.trim();
    (!sentence.is_empty() && !sentence.ends_with(':')).then(|| cap(sentence))
}

/// A Markdown block marker at the start of a (trimmed) line: a heading's `#`s, a list item's `-`,
/// `*`, `+` or `1.`/`1)`, or a quote's `>`.
fn block_marker() -> &'static Regex {
    static MARKER: OnceLock<Regex> = OnceLock::new();
    MARKER
        .get_or_init(|| Regex::new(r"^(?:#{1,6}\s+|[-*+]\s+|>\s?|\d{1,3}[.)]\s+)").expect("valid"))
}

/// The first prose paragraph of a Markdown document, skipping YAML front matter, headings, rules,
/// HTML, fenced code, badge/image lines (a README usually opens with those) and link reference
/// definitions (`[1]: https://…`). As in [`help_description`], a list or quote marker is dropped
/// from a line, and a list after the first line ends the paragraph.
fn first_paragraph(markdown: &str) -> Option<String> {
    static LIST: OnceLock<Regex> = OnceLock::new();
    let list = LIST.get_or_init(|| Regex::new(r"^(?:[-*+]\s+|\d{1,3}[.)]\s+)").expect("valid"));
    let marker = block_marker();
    static REF_DEF: OnceLock<Regex> = OnceLock::new();
    let ref_def = REF_DEF.get_or_init(|| Regex::new(r"^\[[^\]]+\]:\s").expect("valid"));
    let mut lines: Vec<String> = Vec::new();
    let mut in_fence = false;
    // A UTF-8 byte-order mark is no text (`str::trim` keeps it).
    let markdown = markdown.strip_prefix('\u{feff}').unwrap_or(markdown);
    for line in markdown.lines().skip(front_matter_lines(markdown)) {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        if in_fence {
            continue;
        }
        let skip = t.starts_with('#')
            || t.starts_with('<')
            || t.starts_with("![")
            || t.starts_with("[![")
            || t.starts_with('|')
            || ref_def.is_match(t)
            || t.chars().all(|c| matches!(c, '-' | '=' | '*' | '_'));
        if t.is_empty() || skip {
            if !lines.is_empty() {
                break;
            }
            continue;
        }
        if !lines.is_empty() && list.is_match(t) {
            break;
        }
        lines.push(marker.replace(t, "").trim().to_string());
    }
    (!lines.is_empty()).then(|| lines.join(" "))
}

/// How many leading lines are YAML front matter (a static-site docs page's `---` block of
/// `key: value` lines closed by `---` or `...`); 0 when the page has none. A page that merely opens
/// with a `---` rule is not front matter: the block must close, and every line in it must read as
/// YAML (a `key:` line, an indented or `- ` continuation, a comment).
fn front_matter_lines(markdown: &str) -> usize {
    static KEY: OnceLock<Regex> = OnceLock::new();
    let key = KEY.get_or_init(|| Regex::new(r"^[A-Za-z0-9_-]+\s*:(\s|$)").expect("valid"));
    let mut lines = markdown.lines();
    if lines.next().map(|l| l.trim_start_matches('\u{feff}').trim_end()) != Some("---") {
        return 0;
    }
    let mut keys = 0;
    for (i, line) in lines.enumerate() {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            return if keys > 0 { i + 2 } else { 0 };
        }
        if key.is_match(t) {
            keys += 1;
        } else if !(t.is_empty() || t.starts_with([' ', '\t', '#']) || t.starts_with("- ")) {
            return 0;
        }
    }
    0
}

/// Inline Markdown reduced to its text: links (inline `[t](url)` and reference `[t][id]`) and
/// images to their text, autolinks to their address, common inline HTML tags and comments dropped,
/// emphasis and code marks dropped. Only a `<` that opens a known tag is HTML, so prose such as
/// `quality < 20 and length > 100` is kept whole.
fn plain_text(inline: &str) -> String {
    static LINK: OnceLock<Regex> = OnceLock::new();
    static REF_LINK: OnceLock<Regex> = OnceLock::new();
    static AUTOLINK: OnceLock<Regex> = OnceLock::new();
    static HTML: OnceLock<Regex> = OnceLock::new();
    static MARKS: OnceLock<Regex> = OnceLock::new();
    let link = LINK.get_or_init(|| Regex::new(r"!?\[([^\]]*)\]\([^)]*\)").expect("valid"));
    let ref_link = REF_LINK.get_or_init(|| Regex::new(r"!?\[([^\]]*)\]\[[^\]]*\]").expect("valid"));
    let autolink = AUTOLINK.get_or_init(|| {
        Regex::new(r"<((?:https?|ftp)://[^<>\s]+|mailto:[^<>\s]+)>").expect("valid")
    });
    let html = HTML.get_or_init(|| {
        Regex::new(
            r"(?i)<!--.*?-->|</?(?:a|abbr|b|br|code|del|div|em|font|i|img|ins|kbd|mark|p|s|small|span|strong|sub|sup|u)(?:\s[^<>]*)?/?>",
        )
        .expect("valid")
    });
    let marks = MARKS.get_or_init(|| Regex::new(r"\*\*|__|[*`]|\b_|_\b").expect("valid"));
    let text = link.replace_all(inline, "$1");
    let text = ref_link.replace_all(&text, "$1");
    let text = autolink.replace_all(&text, "$1");
    let text = html.replace_all(&text, "");
    marks.replace_all(&text, "").into_owned()
}

/// Abbreviations whose period does not end a sentence (compared lowercased, without the final
/// period).
const ABBREVIATIONS: &[&str] =
    &["e.g", "i.e", "cf", "vs", "approx", "incl", "al", "fig", "ca", "resp"];

/// Text up to and including its first sentence end, or all of it. A sentence ends at `!` or `?`
/// followed by a space or the end, and at a `.` likewise unless the word it ends is a known
/// abbreviation (`e.g.`, `i.e.`, `vs.`) or the next word starts in lower case (`… approx. two
/// hours`): a heuristic, so a sentence that starts in lower case is read as part of the one before.
fn first_sentence(text: &str) -> String {
    let chars: Vec<char> = text.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if !matches!(c, '.' | '!' | '?') || !chars.get(i + 1).is_none_or(|n| n.is_whitespace()) {
            continue;
        }
        if *c == '.' {
            let next = chars[i + 1..].iter().find(|n| !n.is_whitespace());
            if next.is_some_and(|n| n.is_lowercase()) {
                continue;
            }
            let start = chars[..i]
                .iter()
                .rposition(|p| p.is_whitespace() || *p == '(')
                .map_or(0, |p| p + 1);
            let word = chars[start..i].iter().collect::<String>().to_lowercase();
            if ABBREVIATIONS.contains(&word.as_str()) {
                continue;
            }
            // `No. 5` is an abbreviation; `Default: no. Use yes …` ends a sentence.
            if word == "no" && next.is_some_and(char::is_ascii_digit) {
                continue;
            }
        }
        return chars[..=i].iter().collect();
    }
    text.to_string()
}

fn cap(text: &str) -> String {
    if text.chars().count() <= MAX_SUMMARY_CHARS {
        return text.to_string();
    }
    let cut = crate::text::truncate_chars(text, MAX_SUMMARY_CHARS - 1);
    format!("{}…", cut.trim_end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_help_describes_its_param_by_its_first_whole_sentence() {
        let d = |h: &str| help_description(h);
        // Abbreviations do not end a sentence; `!` and `?` do.
        assert_eq!(
            d("Aligner to use, e.g. STAR or HISAT2. STAR suits most.").as_deref(),
            Some("Aligner to use, e.g. STAR or HISAT2.")
        );
        assert_eq!(
            d("Path to the reference (i.e. a FASTA). More.").as_deref(),
            Some("Path to the reference (i.e. a FASTA).")
        );
        assert_eq!(d("Really? Yes.").as_deref(), Some("Really?"));
        assert_eq!(d("Stop now! Then go.").as_deref(), Some("Stop now!"));
        // A hard-wrapped block reads as one line, and a blank line ends the paragraph.
        assert_eq!(
            d("Path to the reference\ngenome FASTA file. Used by all steps.").as_deref(),
            Some("Path to the reference genome FASTA file.")
        );
        assert_eq!(d("No stop here\n\nNext paragraph.").as_deref(), Some("No stop here"));
        // Markdown is reduced to text: a leading marker, emphasis, code and links.
        assert_eq!(
            d("**NB:** uses `STAR` from [here](https://x). More.").as_deref(),
            Some("NB: uses STAR from here.")
        );
        assert_eq!(d("# Aligner\nWhich one.").as_deref(), Some("Aligner"));
        assert_eq!(d("1. Run STAR first\n2. Then Salmon").as_deref(), Some("Run STAR first"));
        assert_eq!(d("- **STAR**: fast. Other.").as_deref(), Some("STAR: fast."));
        // A help that leads into a list gives no description, never a dangling one.
        assert_eq!(d("Choose one of:\n- star\n- hisat2"), None);
        assert_eq!(d("  \n "), None);
    }

    #[test]
    fn the_form_description_wins() {
        assert_eq!(
            summary(Some("  Align reads.  "), Some("Other text.")),
            Some("Align reads.".into())
        );
        assert_eq!(summary(Some(" "), None), None);
        assert_eq!(summary(None, None), None);
    }

    #[test]
    fn the_about_page_gives_its_first_sentence_as_plain_text() {
        let about = "# nf-core/rnaseq\n\n[![CI](https://x/badge.svg)](https://x)\n\n\
                     **RNA-seq** analysis using [STAR](https://star) and\nSalmon. It also \
                     runs QC.\n\nMore.";
        assert_eq!(
            summary(None, Some(about)),
            Some("RNA-seq analysis using STAR and Salmon.".into())
        );
        // Snake-case words keep their underscores.
        assert_eq!(
            summary(None, Some("Reads `fastq_1` files")),
            Some("Reads fastq_1 files".into())
        );
    }

    #[test]
    fn a_page_that_opens_with_a_list_is_read_as_help_is() {
        // A list marker is not text, and a list after the first line ends the paragraph.
        assert_eq!(
            summary(None, Some("1. Run STAR first.\n2. Then Salmon.")),
            Some("Run STAR first.".into())
        );
        assert_eq!(
            summary(None, Some("- Aligns reads.\n- Counts genes.")),
            Some("Aligns reads.".into())
        );
        assert_eq!(summary(None, Some("> Quoted lead.\n> More.")), Some("Quoted lead.".into()));
        // Prose that leads into a list says nothing on its own.
        assert_eq!(summary(None, Some("Choose one of:\n- star\n- hisat2")), None);
    }

    #[test]
    fn a_bom_markdown_in_the_description_and_a_bare_no_are_read_as_text() {
        assert_eq!(
            summary(None, Some("\u{feff}# nf-core/rnaseq\n\nAligns reads.")),
            Some("Aligns reads.".into())
        );
        assert_eq!(
            summary(Some("**Align** reads with [STAR](https://x)."), None),
            Some("Align reads with STAR.".into())
        );
        assert_eq!(
            help_description("Default: no. Use yes to enable."),
            Some("Default: no.".into())
        );
        assert_eq!(
            help_description("Uses chromosome no. 5 only. More."),
            Some("Uses chromosome no. 5 only.".into())
        );
    }

    #[test]
    fn code_and_empty_pages_give_nothing_or_the_prose_after() {
        assert_eq!(summary(None, Some("# Title\n\n---\n")), None);
        assert_eq!(
            summary(None, Some("```\nrun me\n```\nThen this. And more")),
            Some("Then this.".into())
        );
        // A version number is not a sentence end.
        assert_eq!(
            summary(None, Some("Uses v1.2 of the tool. Done.")),
            Some("Uses v1.2 of the tool.".into())
        );
    }

    #[test]
    fn reference_links_autolinks_and_inline_html_become_text() {
        assert_eq!(
            summary(None, Some("Align [reads][1] with [STAR][] <b>fast</b>.\n\n[1]: https://x")),
            Some("Align reads with STAR fast.".into())
        );
        assert_eq!(
            summary(None, Some("See <https://nf-co.re> for docs<br/> and <!-- hidden --> more.")),
            Some("See https://nf-co.re for docs and more.".into())
        );
        // A comparison is prose, not a tag.
        assert_eq!(
            summary(None, Some("Filter reads where quality < 20 and length > 100.")),
            Some("Filter reads where quality < 20 and length > 100.".into())
        );
        // A page that opens with its link definitions still finds its prose.
        assert_eq!(
            summary(None, Some("[star]: https://star\n\nAligns reads.")),
            Some("Aligns reads.".into())
        );
    }

    #[test]
    fn front_matter_is_not_the_summary() {
        let page = "---\ntitle: Align\nlayout: page\ntags:\n  - rna\n---\n\nAligns reads. More.";
        assert_eq!(summary(None, Some(page)), Some("Aligns reads.".into()));
        // An opening rule is not front matter: the prose after it is the summary.
        assert_eq!(
            summary(None, Some("---\n\nAligns reads to a genome. More.\n\n---\nOther text")),
            Some("Aligns reads to a genome.".into())
        );
        // Nor is an unclosed `---` block.
        assert_eq!(front_matter_lines("---\ntitle: x\n\nProse."), 0);
        assert_eq!(front_matter_lines("---\ntitle: x\n...\nProse."), 3);
    }

    #[test]
    fn abbreviations_do_not_end_the_sentence() {
        assert_eq!(
            summary(None, Some("Use e.g. STAR or i.e. Salmon here. More")),
            Some("Use e.g. STAR or i.e. Salmon here.".into())
        );
        assert_eq!(
            summary(None, Some("Takes approx. two hours. More")),
            Some("Takes approx. two hours.".into())
        );
        // `etc.` before a new sentence still ends it; `!` and `?` always do.
        assert_eq!(
            summary(None, Some("Reads BAM, CRAM, etc. It also sorts.")),
            Some("Reads BAM, CRAM, etc.".into())
        );
        assert_eq!(summary(None, Some("Fast! and more")), Some("Fast!".into()));
    }

    #[test]
    fn a_long_sentence_is_cut() {
        let s = summary(None, Some(&"word ".repeat(100))).unwrap();
        assert_eq!(s.chars().count(), MAX_SUMMARY_CHARS);
        assert!(s.ends_with('…'));
    }
}
