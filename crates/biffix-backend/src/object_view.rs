//! One S3 object as the API presents it, wherever it came from (a run's output, a data source's
//! file, a report): its preview, how the browser can show it, and short-lived view and download
//! links. The handlers that serve objects (`outputs`, `data`) each keep only their own
//! authorization, URI resolution and error mapping.

use std::io::Read;
use std::time::Duration;

use biffix_api::{OutputPreview, OutputUrl};
use biffix_workflow::text::basename;

use crate::aws::objects::ObjectStore;
use crate::aws::AwsError;

/// How long a download or view link works. Well inside the Lambda role's session lifetime, which
/// bounds a presigned URL's real validity regardless of what it claims.
pub(crate) const LINK_EXPIRY: Duration = Duration::from_secs(600);
/// How long a stream link works: a genome browser reads a file in Range requests for as long as it
/// stays open. The Lambda role's session can end sooner (a presigned URL dies with the credentials
/// that signed it), so the client asks for fresh links shortly before `expires_at`, and again
/// whenever a read is refused (a 403, before the viewer starts or while it runs), renewing with
/// `?locus=false` since only the links change.
pub(crate) const STREAM_LINK_EXPIRY: Duration = Duration::from_secs(3600);

/// Preview size. The decompressed text is capped at the same size, so a gzip bomb costs at most
/// this much memory.
pub(crate) const PREVIEW_BYTES: u64 = 1 << 20;
/// Leading bytes inspected to tell text from binary.
const SNIFF_BYTES: usize = 8 << 10;

/// Which link to an object: shown inline in the browser, downloaded as an attachment, or read by a
/// client in Range requests (a genome browser): no content type or disposition is fixed, so S3
/// answers each range as it stores the object.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Link {
    View,
    Download,
    Stream,
}

/// Why a link could not be made.
#[derive(Debug)]
pub(crate) enum LinkError {
    /// A view link was asked for a file the browser cannot show (by its name; a 409): download it
    /// instead. Carries the message.
    NoViewer(String),
    Aws(AwsError),
}

/// A short-lived presigned URL for `uri`. A view link fixes the content type from the file's name
/// (never what S3 stored: uploads are often `binary/octet-stream`); a download link is an
/// attachment; a stream link fixes neither and lasts longer ([`STREAM_LINK_EXPIRY`]).
pub(crate) async fn presigned_link(
    cloud: &dyn ObjectStore,
    uri: &str,
    link: Link,
) -> Result<OutputUrl, LinkError> {
    let filename = basename(uri).to_string();
    let (disposition, content_type) = match link {
        Link::View => match view_content_type(&filename) {
            Some(t) => (Some("inline"), Some(t)),
            None => {
                return Err(LinkError::NoViewer(format!(
                    "{filename} cannot be shown in the browser; download it"
                )))
            }
        },
        Link::Download => (Some("attachment"), None),
        Link::Stream => (None, None),
    };
    let expiry = match link {
        Link::Stream => STREAM_LINK_EXPIRY,
        Link::View | Link::Download => LINK_EXPIRY,
    };
    let disposition = disposition.map(|d| content_disposition(d, &filename));
    let url = cloud
        .presign_get(uri, expiry, disposition.as_deref(), content_type)
        .await
        .map_err(LinkError::Aws)?;
    Ok(OutputUrl {
        url,
        expires_at: biffix_core::clock::epoch_secs() + expiry.as_secs_f64(),
        filename,
        content_type: content_type.map(str::to_string),
    })
}

/// The start of `uri` as a preview ([`decode_preview`]): [`PREVIEW_BYTES`] read from its first byte.
/// The whole object at `uri` when it is at most `cap` bytes; `Ok(None)` when S3 says it is larger.
pub(crate) async fn read_capped(
    cloud: &dyn ObjectStore,
    uri: &str,
    cap: u64,
) -> Result<Option<Vec<u8>>, AwsError> {
    let body = cloud.get_object_range(uri, 0, cap - 1).await?;
    Ok((!body.total_size.is_some_and(|total| total > cap)).then_some(body.bytes))
}

pub(crate) async fn preview(cloud: &dyn ObjectStore, uri: &str) -> Result<OutputPreview, AwsError> {
    let body = cloud.get_object_range(uri, 0, PREVIEW_BYTES - 1).await?;
    Ok(decode_preview(&body.bytes, PREVIEW_BYTES as usize, body.total_size))
}

/// Turn the first bytes of an object into a preview: inflate gzip (multi-member, so bgzip too) up to
/// `cap` bytes of output, then report text, or `binary` when the bytes are not text. `total_size` is
/// the object's full size, when known, so a short read is not mistaken for the whole file.
pub(crate) fn decode_preview(raw: &[u8], cap: usize, total_size: Option<u64>) -> OutputPreview {
    let raw_truncated = total_size.map_or(raw.len() >= cap, |total| total > raw.len() as u64);
    let decompressed = raw.starts_with(&[0x1f, 0x8b]);
    let (data, truncated) = if decompressed {
        let mut out = Vec::new();
        // One byte past the cap tells a file that fills it exactly from one that does not fit.
        // A read error (the input was cut short by the range read, or is corrupt) keeps what was
        // decoded so far.
        let result =
            flate2::read::MultiGzDecoder::new(raw).take(cap as u64 + 1).read_to_end(&mut out);
        let hit_cap = out.len() > cap;
        out.truncate(cap);
        (out, raw_truncated || hit_cap || result.is_err())
    } else {
        (raw.to_vec(), raw_truncated)
    };
    let binary = looks_binary(&data);
    let text = if binary {
        String::new()
    } else {
        String::from_utf8_lossy(trim_partial_utf8(&data)).into_owned()
    };
    OutputPreview {
        kind: if binary { "binary" } else { "text" }.to_string(),
        text,
        truncated,
        bytes_read: raw.len() as i64,
        total_size: total_size.map(|t| t as i64),
        decompressed,
    }
}

/// Binary when the leading bytes hold a NUL, or when over a tenth of their characters are not valid
/// UTF-8.
fn looks_binary(data: &[u8]) -> bool {
    let sample = trim_partial_utf8(&data[..data.len().min(SNIFF_BYTES)]);
    if sample.contains(&0) {
        return true;
    }
    let text = String::from_utf8_lossy(sample);
    let invalid = text.chars().filter(|&c| c == char::REPLACEMENT_CHARACTER).count();
    invalid * 10 > text.chars().count()
}

/// `data` without a UTF-8 sequence cut off at its end (a read that stopped mid-character).
fn trim_partial_utf8(data: &[u8]) -> &[u8] {
    match std::str::from_utf8(data) {
        Err(e) if e.error_len().is_none() => &data[..e.valid_up_to()],
        _ => data,
    }
}

/// The content type a view URL serves `name` as. Only image, PDF and HTML outputs have one — text is
/// previewed through the backend instead. Fixed here, never taken from S3's stored type.
pub(crate) fn view_content_type(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map(|(_, ext)| ext)?;
    Some(match ext {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "pdf" => "application/pdf",
        "html" | "htm" => "text/html; charset=utf-8",
        _ => return None,
    })
}

/// A `Content-Disposition` value (`kind` is `attachment` or `inline`) for `filename`: a plain
/// `filename=` with quotes, backslashes and control characters removed (and non-ASCII replaced) for
/// old clients, plus an RFC 5987 `filename*=` carrying the exact UTF-8 name when it is not plain
/// ASCII.
pub(crate) fn content_disposition(kind: &str, filename: &str) -> String {
    let ascii: String = filename
        .chars()
        .filter(|c| !c.is_control() && *c != '"' && *c != '\\')
        .map(|c| if c.is_ascii() { c } else { '_' })
        .collect();
    let ascii = if ascii.trim().is_empty() { "download".to_string() } else { ascii };
    if filename.is_ascii() && ascii == filename {
        return format!("{kind}; filename=\"{ascii}\"");
    }
    // RFC 5987: what is not an attr-char percent-encoded. `urlencoding` leaves only the unreserved
    // characters (a subset of attr-char) as they are, which is as valid.
    let encoded = urlencoding::encode(filename);
    format!("{kind}; filename=\"{ascii}\"; filename*=UTF-8''{encoded}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gzip(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut enc = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    #[test]
    fn preview_of_a_whole_small_text_file() {
        let p = decode_preview(b"a\tb\n1\t2\n", 1024, Some(8));
        assert_eq!(p.kind, "text");
        assert_eq!(p.text, "a\tb\n1\t2\n");
        assert!(!p.truncated);
        assert!(!p.decompressed);
        assert_eq!(p.bytes_read, 8);
        assert_eq!(p.total_size, Some(8));
    }

    #[test]
    fn preview_marks_a_longer_file_truncated_and_drops_a_split_character() {
        // "é" is two bytes; the read stops between them.
        let raw = "abcé".as_bytes();
        let p = decode_preview(&raw[..4], 4, Some(raw.len() as u64));
        assert!(p.truncated);
        assert_eq!(p.text, "abc", "the half character is dropped, not replaced");
        // Without a known size, a read that filled the cap is assumed to be cut short.
        assert!(decode_preview(b"abcd", 4, None).truncated);
        assert!(!decode_preview(b"abc", 4, None).truncated);
    }

    #[test]
    fn preview_inflates_gzip_and_multi_member_gzip() {
        let one = gzip(b"hello\n");
        let p = decode_preview(&one, 1024, Some(one.len() as u64));
        assert!(p.decompressed);
        assert_eq!(p.text, "hello\n");
        assert!(!p.truncated);

        // bgzip is a series of gzip members.
        let mut two = gzip(b"first\n");
        two.extend(gzip(b"second\n"));
        let p = decode_preview(&two, 1024, Some(two.len() as u64));
        assert_eq!(p.text, "first\nsecond\n");
    }

    #[test]
    fn preview_caps_decompressed_output() {
        let big = gzip(&vec![b'x'; 10_000]);
        let p = decode_preview(&big, 100, Some(big.len() as u64));
        assert_eq!(p.text.len(), 100, "a gzip bomb is bounded by the cap");
        assert!(p.truncated);
    }

    #[test]
    fn preview_of_a_cut_short_gzip_keeps_what_decoded() {
        let data: Vec<u8> = (0..20_000).map(|i| b"ACGT\n"[i % 5]).collect();
        let whole = gzip(&data);
        let half = &whole[..whole.len() / 2];
        let p = decode_preview(half, half.len(), Some(whole.len() as u64));
        assert!(p.truncated);
        assert_eq!(p.kind, "text");
        assert!(!p.text.is_empty());
        assert!(data.starts_with(p.text.as_bytes()));
    }

    #[test]
    fn preview_reports_binary_without_text() {
        let p = decode_preview(b"BAM\x01\x00\x00\x00", 1024, Some(7));
        assert_eq!(p.kind, "binary");
        assert!(p.text.is_empty());
        // Mostly invalid UTF-8 with no NUL is binary too; a stray bad byte in text is not.
        assert_eq!(decode_preview(&[0xff; 64], 1024, Some(64)).kind, "binary");
        let mut mostly_text = b"a\tb\n".repeat(20);
        mostly_text.push(0xff);
        assert_eq!(decode_preview(&mostly_text, 1024, None).kind, "text");
    }

    #[test]
    fn view_content_type_only_for_inline_viewers() {
        assert_eq!(view_content_type("a.PNG"), Some("image/png"));
        assert_eq!(view_content_type("a.jpeg"), Some("image/jpeg"));
        assert_eq!(view_content_type("r.pdf"), Some("application/pdf"));
        assert_eq!(view_content_type("r.htm"), Some("text/html; charset=utf-8"));
        assert_eq!(view_content_type("m.tsv"), None);
        assert_eq!(view_content_type("a.bam"), None);
    }

    #[test]
    fn content_disposition_sanitizes_and_encodes() {
        assert_eq!(content_disposition("attachment", "a.bam"), "attachment; filename=\"a.bam\"");
        assert_eq!(
            content_disposition("attachment", "we\"ird\\name\n.txt"),
            "attachment; filename=\"weirdname.txt\"; \
             filename*=UTF-8''we%22ird%5Cname%0A.txt"
        );
        assert_eq!(
            content_disposition("attachment", "résumé.pdf"),
            "attachment; filename=\"r_sum_.pdf\"; filename*=UTF-8''r%C3%A9sum%C3%A9.pdf"
        );
        assert_eq!(
            content_disposition("attachment", "\"\""),
            "attachment; filename=\"download\"; filename*=UTF-8''%22%22"
        );
    }
}
