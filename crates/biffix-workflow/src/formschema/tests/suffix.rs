use serde_json::json;

use super::*;
use crate::manifest::BiffixManifest;

#[test]
fn the_suffix_hint_lists_accepted_endings() {
    let manifest = BiffixManifest::parse_yaml(
        "params:\n  reads: { format: s3-file, suffix: '.fastq.gz, .fq.gz' }\n",
    )
    .unwrap();
    let fs = sm_adapt(&json!({"properties": {"reads": {"type": "string"}}}), &manifest);
    assert_eq!(field(&fs, "reads").suffixes, [".fastq.gz", ".fq.gz"]);
}
