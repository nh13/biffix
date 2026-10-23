//! The effective output prefix, run outputs and the output-set digest.

use super::*;

/// An anchor recording `prefix`. The real anchor key is one per run, so a real log holds one anchor;
/// the per-prefix key lets a test put two conflicting anchors in front of the fold.
fn run_submitted_with_prefix(ts: f64, run: &str, prefix: &str) -> RunEvent {
    let mut e = run_anchor(ts, run);
    if let EventData::RunSubmitted(anchor) = &mut e.data {
        anchor.output_prefix = Some(prefix.into());
    }
    e.dedup_key = format!("anchor-{prefix}");
    e
}

#[test]
fn anchor_without_output_prefix_projects_none() {
    // An anchor recorded before the field existed: readers fall back to the derived default.
    let p = Projection::replay(&[run_anchor(1.0, "r1")]);
    assert_eq!(p.runs["r1"].output_prefix, None);
}

#[test]
fn anchor_output_prefix_is_projected() {
    let p = Projection::replay(&[run_submitted_with_prefix(1.0, "r1", "s3://b/")]);
    assert_eq!(p.runs["r1"].output_prefix.as_deref(), Some("s3://b/"));
}

#[test]
fn output_prefix_is_set_once_from_the_earliest_anchor() {
    // Two anchors (a redelivered-with-a-difference edge case; the dedup key admits one per run):
    // the earliest supplies every anchor field — even one only the later anchor sets — in whichever
    // order the two are folded, so the fold is a pure function of the event set.
    let first = run_submitted_with_prefix(1.0, "r1", "s3://b/first/");
    let mut later = run_submitted_with_prefix(2.0, "r1", "s3://b/later/");
    if let EventData::RunSubmitted(anchor) = &mut later.data {
        anchor.run_name = Some("later-name".into());
    }
    for log in [vec![first.clone(), later.clone()], vec![later, first]] {
        let p = fold_in_order(&log);
        let rp = &p.runs["r1"];
        assert_eq!(rp.output_prefix.as_deref(), Some("s3://b/first/"));
        assert_eq!(rp.run_name, None, "anchor fields never mix across anchors");
        assert_eq!(rp.submitted_at_micros, Some(1_000_000));
    }
}

fn task_outputs(
    ts: f64,
    run: &str,
    task: Option<&str>,
    attempt: Option<u32>,
    role: Option<biffix_core::event::OutputRole>,
    reused: bool,
    refs: &[(&str, Option<&str>)],
) -> RunEvent {
    use biffix_core::event::OutputRef;
    let mut e = ev(
        ts,
        EventSource::Snakemake,
        EventData::TaskOutputs {
            role,
            reused,
            outputs: refs
                .iter()
                .map(|(uri, path)| OutputRef {
                    s3_uri: uri.to_string(),
                    path: path.map(str::to_string),
                    etag: None,
                    size: None,
                })
                .collect(),
        },
    );
    e.run_id = Some(RunId::new(run));
    e.task_key = task.map(TaskKey::new);
    e.attempt = attempt.map(Attempt);
    e
}

#[test]
fn task_outputs_fold_into_the_run_output_set() {
    use biffix_core::event::OutputRole;
    let log = vec![
        run_anchor(1.0, "r1"),
        task_outputs(
            2.0,
            "r1",
            Some("align:s=a"),
            Some(1),
            None,
            false,
            &[("s3://b/p/a.bam", None), ("s3://b/p/a.bai", None)],
        ),
        task_outputs(
            3.0,
            "r1",
            Some("align:s=a"),
            Some(1),
            Some(OutputRole::Log),
            false,
            &[("s3://b/p/logs/a.log", None)],
        ),
        task_outputs(
            4.0,
            "r1",
            Some("work"),
            Some(1),
            Some(OutputRole::Intermediate),
            false,
            &[("s3://b/p/work/ab/cd/x", None)],
        ),
    ];
    let p = Projection::replay(&log);
    let rp = &p.runs["r1"];
    assert_eq!(rp.outputs.len(), 4);
    let bam = &rp.outputs["s3://b/p/a.bam"];
    assert_eq!(bam.task_key.as_deref(), Some("align:s=a"));
    assert_eq!(bam.role, OutputRole::Output, "no role on the event means output");
    assert_eq!(rp.outputs["s3://b/p/logs/a.log"].role, OutputRole::Log);
    // Intermediates are recorded but are not "the run's outputs".
    assert_eq!(rp.outputs_total(), 3);
    assert_eq!(rp.outputs_reused(), 0);
}

#[test]
fn output_merge_rules_are_order_independent() {
    use biffix_core::event::OutputRole;
    // One object reported three ways: as a reused intermediate by attempt 1, as a published file
    // (run-level, with a path) and as produced by attempt 2. Every permutation must converge on:
    // published role, attempt 2, not reused, the task credited, the path kept.
    let log = vec![
        run_anchor(1.0, "r1"),
        task_outputs(
            2.0,
            "r1",
            Some("p (1)"),
            Some(1),
            Some(OutputRole::Intermediate),
            true,
            &[("s3://b/x.txt", None)],
        ),
        task_outputs(
            2.0,
            "r1",
            None,
            None,
            Some(OutputRole::Published),
            true,
            &[("s3://b/x.txt", Some("results/x.txt"))],
        ),
        task_outputs(
            2.0,
            "r1",
            Some("p (1)"),
            Some(2),
            Some(OutputRole::Intermediate),
            false,
            &[("s3://b/x.txt", None)],
        ),
    ];
    assert_incremental_equals_replay(&log);
    let o = &Projection::replay(&log).runs["r1"].outputs["s3://b/x.txt"];
    assert_eq!(o.role, OutputRole::Published);
    assert_eq!(o.attempt, Some(Attempt(2)));
    assert!(!o.reused, "one produced claim beats any number of reused ones");
    assert_eq!(o.task_key.as_deref(), Some("p (1)"));
    assert_eq!(o.path.as_deref(), Some("results/x.txt"));
}

#[test]
fn a_reused_only_output_counts_as_reused() {
    let log = vec![
        run_anchor(1.0, "r1"),
        task_outputs(2.0, "r1", Some("t"), None, None, true, &[("s3://b/y", None)]),
    ];
    let rp = &Projection::replay(&log).runs["r1"];
    assert_eq!((rp.outputs_total(), rp.outputs_reused()), (1, 1));
}

#[test]
fn the_anchor_s_reference_bundle_lands_on_the_run() {
    let mut anchor = run_anchor(1.0, "r1");
    let bundle = biffix_core::event::ReferenceUse {
        id: "grch38".into(),
        uris: [("fasta".to_string(), "s3://refs/hg38.fa".to_string())].into(),
    };
    if let EventData::RunSubmitted(submitted) = &mut anchor.data {
        submitted.reference_bundle = Some(bundle.clone());
    }
    let mut p = Projection::new();
    p.apply(&anchor);
    assert_eq!(p.runs["r1"].reference_bundle, Some(bundle));
}

#[test]
fn work_deleted_folds_to_its_earliest_time_in_any_order() {
    let mut succeeded = run_level(3.0, "r1", EventData::RunSucceeded {});
    succeeded.dedup_key = "succeeded".into();
    let mut deleted = control(5.0, "r1", EventData::WorkDeleted {});
    deleted.dedup_key = "deleted-5".into();
    // A second sweep result, later: the first recorded time stands. `work_deleted` has one key per
    // run, so a real log keeps only one; the hand-set keys probe the fold's min rule should two
    // reach it.
    let mut again = control(9.0, "r1", EventData::WorkDeleted {});
    again.dedup_key = "deleted-9".into();
    let log = vec![run_anchor(1.0, "r1"), succeeded, deleted, again];
    assert_incremental_equals_replay(&log);
    let p = Projection::replay(&log);
    let run = &p.runs["r1"];
    assert_eq!(run.work_deleted_at_micros, Some(5_000_000));
    assert_eq!(run_state(&p, "r1"), RunState::Succeeded);
}

#[test]
fn outputs_digest_changes_with_every_output_field_and_only_with_them() {
    use biffix_core::event::OutputRole;
    let base = || {
        let log = vec![run_anchor(1.0, "r1"), output_ref_event(2.0, "x.txt", "etag-1", 10)];
        Projection::replay(&log).runs["r1"].clone()
    };
    let digest = base().outputs_digest();
    assert_eq!(digest, base().outputs_digest(), "equal sets, equal digests");
    // Fields outside the output set do not move it.
    let mut other = base();
    other.run_name = Some("renamed".into());
    assert_eq!(other.outputs_digest(), digest);

    type Mutation = fn(&mut biffix_core::reducer::OutputProjection);
    let mutations: [(&str, Mutation); 8] = [
        ("s3_uri", |o| o.s3_uri = "s3://b/y.txt".into()),
        ("path", |o| o.path = Some("y.txt".into())),
        ("task_key", |o| o.task_key = Some("t".into())),
        ("attempt", |o| o.attempt = Some(Attempt(2))),
        ("role", |o| o.role = OutputRole::Log),
        ("reused", |o| o.reused = !o.reused),
        ("etag", |o| o.etag = Some("etag-2".into())),
        ("size", |o| o.size = Some(11)),
    ];
    for (field, mutate) in mutations {
        let mut changed = base();
        mutate(changed.outputs.get_mut("s3://b/x.txt").unwrap());
        assert_ne!(changed.outputs_digest(), digest, "{field} must change the digest");
    }
    // Adding or removing an object changes it too.
    let mut fewer = base();
    fewer.outputs.clear();
    assert_ne!(fewer.outputs_digest(), digest);
}

#[test]
fn a_terminal_log_stream_wins_over_the_started_one_in_any_order() {
    // A Batch retry inside one job starts a new container; the terminal names the final one. The
    // terminal's stream is neither the lexicographic min, the max, nor the latest-reported one (a
    // late start names another), so only terminal-wins gives it.
    let started = batch(
        3.0,
        "job-1",
        EventData::TaskStarted { started_at: None, log_stream: Some("a-first-container".into()) },
    );
    let done = batch(
        4.0,
        "job-1",
        EventData::TaskSucceeded { exit_code: 0, log_stream: Some("m-final-container".into()) },
    );
    let late = batch(
        5.0,
        "job-1",
        EventData::TaskStarted { started_at: None, log_stream: Some("z-late-container".into()) },
    );
    let base = [run_anchor(1.0, "r1"), task_submitted(2.0, "r1", "align", 1, "job-1")];
    for tail in permutations(&[started, done, late]) {
        let log: Vec<RunEvent> = base.iter().cloned().chain(tail).collect();
        let p = fold_in_order(&log);
        let attempt = &p.runs["r1"].tasks["align"].attempts["job-1"];
        assert_eq!(attempt.log_stream.as_deref(), Some("m-final-container"));
    }
}
