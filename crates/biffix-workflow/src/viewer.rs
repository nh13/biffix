//! How the UI can show a file in the browser, from its name: the one filename-extension table
//! behind the outputs and data browsers (their `viewer`) and the run's key results
//! ([`crate::formschema::report_format`]).

/// How the UI can show a file named `name`: `table` (delimited text), `text`, `image`, `pdf` or
/// `html`. `None` ⇒ download only. A `.gz` suffix is looked through for the text kinds, since the
/// preview decompresses.
pub fn viewer_of(name: &str) -> Option<&'static str> {
    let lower = name.to_ascii_lowercase();
    let (base, gz) = match lower.strip_suffix(".gz") {
        Some(base) => (base, true),
        None => (lower.as_str(), false),
    };
    let ext = base.rsplit_once('.').map(|(_, ext)| ext)?;
    let viewer = match ext {
        "tsv" | "csv" | "txt" | "metrics" | "bed" | "gtf" | "gff" | "gff3" | "vcf" => "table",
        "log" | "json" | "yaml" | "yml" | "md" | "fa" | "fasta" | "fq" | "fastq" | "sam"
        | "out" | "err" => "text",
        "png" | "jpg" | "jpeg" | "gif" | "svg" | "webp" if !gz => "image",
        "pdf" if !gz => "pdf",
        "html" | "htm" if !gz => "html",
        _ => return None,
    };
    Some(viewer)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn viewer_by_name() {
        for (name, viewer) in [
            ("metrics.tsv", Some("table")),
            ("x.umi_grouping.metrics", Some("table")),
            ("calls.vcf.gz", Some("table")),
            ("Data.CSV", Some("table")),
            ("run.log", Some("text")),
            ("reads.fastq.gz", Some("text")),
            ("plot.png", Some("image")),
            ("plot.svg", Some("image")),
            ("report.pdf", Some("pdf")),
            ("multiqc_report.html", Some("html")),
            ("report.html.gz", None),
            ("aligned.bam", None),
            ("aligned.bam.bai", None),
            ("README", None),
        ] {
            assert_eq!(viewer_of(name), viewer, "{name}");
        }
    }
}
