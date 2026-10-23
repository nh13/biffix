//! Properties of sample detection over generated file names.

use biffix_workflow::samples::{detect, DetectFile, DetectRule, IdMethod, Lanes};
use proptest::prelude::*;

/// One or two file names in the schemes detection reads (or none): a pair is an uncompressed
/// file given beside its gzipped copy, which detection skips.
fn name() -> impl Strategy<Value = Vec<String>> {
    let id = "[A-Za-z][A-Za-z0-9]{0,5}";
    prop_oneof![
        (id, 1u8..4, 1u8..3, 1u8..3)
            .prop_map(|(i, s, l, r)| vec![format!("{i}_S{s}_L00{l}_R{r}_001.fastq.gz")]),
        (id, 1u8..3).prop_map(|(i, r)| vec![format!("{i}_R{r}.fq.gz")]),
        (id, 1u8..3).prop_map(|(i, r)| vec![format!("{i}_{r}.fastq.gz")]),
        id.prop_map(|i| vec![format!("{i}.fastq.gz")]),
        id.prop_map(|i| vec![format!("{i}_R1.fastq")]),
        (id, 1u8..3).prop_map(|(i, r)| vec![format!("{i}/r{r}.fq.gz")]),
        // A lane in a short name, and a name whose id itself ends like a lane (`A_L001`), so a
        // lane-split id can meet a sample's own.
        (id, 1u8..3, 1u8..3).prop_map(|(i, l, r)| vec![format!("{i}_L00{l}_R{r}.fq.gz")]),
        (id, 1u8..3).prop_map(|(i, l)| vec![format!("{i}_L00{l}_1.fq.gz")]),
        // A whole paired sample split over two lanes (independent draws seldom share an id), so
        // each read list holds several files whose order and mates matter.
        (id, 1u8..4).prop_map(|(i, s)| {
            [(1, 1), (1, 2), (2, 1), (2, 2)]
                .iter()
                .map(|(l, r)| format!("{i}_S{s}_L00{l}_R{r}_001.fastq.gz"))
                .collect()
        }),
        // An index read (skipped).
        (id, 1u8..3).prop_map(|(i, r)| vec![format!("{i}_S1_L001_I{r}_001.fastq.gz")]),
        // An uncompressed file and its gzipped copy (the uncompressed one is skipped).
        (id, 1u8..3, prop_oneof![Just("fastq"), Just("fq")]).prop_map(|(i, r, ext)| {
            vec![format!("{i}_R{r}.{ext}"), format!("{i}_R{r}.{ext}.gz")]
        }),
    ]
}

/// A set of distinct names drawn from `size` [`name`] draws.
fn names(size: std::ops::Range<usize>) -> impl Strategy<Value = Vec<String>> {
    proptest::collection::vec(name(), size).prop_map(|groups| {
        let set: std::collections::BTreeSet<String> = groups.into_iter().flatten().collect();
        set.into_iter().collect()
    })
}

/// Whether detection must skip `name` within `names`: an index read, or an uncompressed file whose
/// gzipped copy is also given.
fn must_skip(name: &str, names: &[String]) -> bool {
    let index = regex::Regex::new(r"_I[12]_\d{3}\.f").unwrap();
    let uncompressed = name.ends_with(".fastq") || name.ends_with(".fq");
    index.is_match(name) || (uncompressed && names.contains(&format!("{name}.gz")))
}

fn files(names: &[String]) -> Vec<DetectFile> {
    names.iter().map(|n| DetectFile { uri: format!("s3://b/run/{n}"), size: None }).collect()
}

fn rule() -> impl Strategy<Value = DetectRule> {
    (
        prop_oneof![Just(Lanes::Merge), Just(Lanes::Separate), Just(Lanes::Error)],
        prop_oneof![
            Just(IdMethod::Auto),
            proptest::collection::vec(0usize..6, 1..3)
                .prop_map(|selected| IdMethod::Tokens { selected }),
        ],
    )
        .prop_map(|(lanes, method)| DetectRule {
            lanes: Some(lanes),
            method,
            ..DetectRule::default()
        })
}

proptest! {
    /// Every distinct input file lands in exactly one place: one sample's reads, unmatched, or
    /// skipped (an index read, or an uncompressed copy of a gzipped file), and every file that
    /// must be skipped is. Ids stay unique under every lanes rule.
    #[test]
    fn every_file_lands_exactly_once(names in names(0..24), rule in rule()) {
        let d = detect(&files(&names), &rule).unwrap();
        for (n, f) in names.iter().zip(files(&names)) {
            prop_assert_eq!(d.skipped.contains(&f.uri), must_skip(n, &names), "{}", n);
        }
        let mut placed: Vec<String> = d
            .samples
            .iter()
            .flat_map(|s| s.read1.iter().chain(s.read2.iter()).cloned())
            .chain(d.unmatched.iter().cloned())
            .chain(d.skipped.iter().cloned())
            .collect();
        placed.sort();
        let mut expected: Vec<String> = files(&names).into_iter().map(|f| f.uri).collect();
        expected.sort();
        prop_assert_eq!(placed, expected);
        // Sample ids are unique.
        let mut ids: Vec<&str> = d.samples.iter().map(|s| s.id.as_str()).collect();
        let n = ids.len();
        ids.sort();
        ids.dedup();
        prop_assert_eq!(ids.len(), n);
    }

    /// A file given twice is read once; the repeat is skipped, not lost.
    #[test]
    fn a_repeated_file_is_read_once_and_the_repeat_skipped(names in names(1..12), rule in rule()) {
        let mut given = files(&names);
        let once = detect(&given, &rule).unwrap();
        given.push(given[0].clone());
        let twice = detect(&given, &rule).unwrap();
        prop_assert_eq!(&twice.samples, &once.samples);
        prop_assert_eq!(&twice.unmatched, &once.unmatched);
        prop_assert_eq!(twice.skipped.len(), once.skipped.len() + 1);
        prop_assert!(twice.skipped.contains(&given[0].uri));
    }

    /// Swapping read 1 and read 2 in every name swaps each sample's read lists, file for file,
    /// under every lanes rule. (Only automatic ids: a token rule may pick the read marker itself
    /// as the id, which the swap then changes.)
    #[test]
    fn pairing_is_symmetric(
        names in names(0..16),
        rule in rule().prop_map(|r| DetectRule { method: IdMethod::Auto, ..r }),
    ) {
        // Only names that carry a read number (a single-end name has nothing to swap).
        let paired_scheme = regex::Regex::new(r"(_R[12]|_[12])\.f").unwrap();
        let names: Vec<String> = names
            .into_iter()
            .filter(|n| paired_scheme.is_match(n) || n.contains("_R1_001") || n.contains("_R2_001"))
            .collect();
        let read_marker = regex::Regex::new(r"_(R?)([12])([._])").unwrap();
        let swap = |n: &String| {
            read_marker
                .replace(n, |c: &regex::Captures| {
                    let other = if &c[2] == "1" { "2" } else { "1" };
                    format!("_{}{other}{}", &c[1], &c[3])
                })
                .into_owned()
        };
        let swapped: Vec<String> = names.iter().map(swap).collect();
        let a = detect(&files(&names), &rule).unwrap();
        let b = detect(&files(&swapped), &rule).unwrap();
        prop_assert_eq!(a.samples.len(), b.samples.len());
        // The swap rewrites only a read marker in the name, never the `s3://b/run/` prefix.
        let swapped_uris = |uris: &[String]| uris.iter().map(swap).collect::<Vec<String>>();
        for (x, y) in a.samples.iter().zip(b.samples.iter()) {
            prop_assert_eq!(&x.id, &y.id);
            prop_assert_eq!(&y.read2, &swapped_uris(&x.read1));
            prop_assert_eq!(&y.read1, &swapped_uris(&x.read2));
        }
    }

    /// Files named only by their read pair up under their folder's name.
    #[test]
    fn read_only_names_pair_by_folder(
        folders in proptest::collection::btree_set("[A-Za-z][A-Za-z0-9]{0,5}", 1..8),
    ) {
        let names: Vec<String> = folders
            .iter()
            .flat_map(|f| [format!("{f}/r1.fq.gz"), format!("{f}/R2.fq.gz")])
            .collect();
        let d = detect(&files(&names), &DetectRule::default()).unwrap();
        let mut ids: Vec<&str> = d.samples.iter().map(|s| s.id.as_str()).collect();
        ids.sort_unstable();
        let mut expected: Vec<&str> = folders.iter().map(String::as_str).collect();
        expected.sort_unstable();
        prop_assert_eq!(ids, expected);
        for s in &d.samples {
            prop_assert_eq!((s.read1.len(), s.read2.len()), (1, 1));
            prop_assert!(s.problems.is_empty());
        }
    }
}
