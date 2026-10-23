use std::collections::{BTreeMap, BTreeSet};

use super::*;
use crate::event::{ReusedSample, SampleHint, SampleSpec, StepScope};

fn spec(ids: &[&str]) -> SampleSpec {
    SampleSpec {
        ids: ids.iter().map(std::string::ToString::to_string).collect(),
        ..SampleSpec::default()
    }
}

#[test]
fn a_sample_state_s_as_str_is_its_serde_name() {
    for state in SampleState::ALL {
        assert_eq!(serde_json::to_value(state).unwrap(), serde_json::json!(state.as_str()));
    }
}

#[test]
fn snakemake_keys_name_their_sample_wildcard() {
    let open = spec(&[]);
    assert_eq!(
        attribute("align:lane=1,sample=HG001", Some("align"), &open).as_deref(),
        Some("HG001")
    );
    assert_eq!(attribute("multiqc", Some("multiqc"), &open), None);
    // No sample-ish wildcard: only a value that is a known id counts.
    let known = spec(&["P07"]);
    assert_eq!(attribute("trim:lib=P07,read=R1", Some("trim"), &known).as_deref(), Some("P07"));
    assert_eq!(attribute("trim:lib=P07,read=R1", Some("trim"), &open), None);
    // Two known ids in one key: a whole-study (pairwise) task.
    let two = spec(&["A", "B"]);
    assert_eq!(attribute("compare:x=A,y=B", Some("compare"), &two), None);
}

#[test]
fn with_a_sheet_a_sample_wildcard_names_only_a_sheet_id() {
    // `id` is a sample wildcard name, but GRCh38 is no sample of the sheet.
    let sheet = spec(&["A"]);
    assert_eq!(attribute("dl:id=GRCh38", Some("dl"), &sheet), None);
    // Falls through to the one sheet id among the values.
    assert_eq!(attribute("x:id=GRCh38,lib=A", Some("x"), &sheet).as_deref(), Some("A"));
    assert_eq!(attribute("dl:id=A", Some("dl"), &sheet).as_deref(), Some("A"));
    // A hinted wildcard is trusted as given.
    let mut hinted = spec(&["A"]);
    hinted.hint.wildcard = Some("id".into());
    assert_eq!(attribute("dl:id=GRCh38", Some("dl"), &hinted).as_deref(), Some("GRCh38"));
}

#[test]
fn a_hinted_wildcard_wins() {
    let mut s = spec(&[]);
    s.hint.wildcard = Some("donor".into());
    assert_eq!(attribute("call:donor=D1,sample=S1", Some("call"), &s).as_deref(), Some("D1"));
    assert_eq!(attribute("call:sample=S1", Some("call"), &s), None);
}

#[test]
fn nextflow_tags_name_their_sample() {
    let open = spec(&[]);
    let key = "NFCORE_RNASEQ:RNASEQ:FASTQC (WT_REP1) #3";
    assert_eq!(attribute(key, Some("FASTQC"), &open).as_deref(), Some("WT_REP1"));
    // Untagged: `process (index)`.
    assert_eq!(attribute("MULTIQC (1)", Some("MULTIQC"), &open), None);
    // A tag holding a known id among other tokens.
    let known = spec(&["WT_REP1", "WT_REP2"]);
    let key = "STAR_ALIGN (WT_REP1.markdup) #7";
    assert_eq!(attribute(key, Some("STAR_ALIGN"), &known).as_deref(), Some("WT_REP1"));
    // A tag that names no known id.
    assert_eq!(attribute("INDEX (genome.fa) #1", Some("INDEX"), &known), None);
}

#[test]
fn a_numeric_tag_is_a_tag_and_only_an_untagged_index_is_not() {
    let open = spec(&[]);
    assert_eq!(attribute("FASTQC (1234) #5", Some("FASTQC"), &open).as_deref(), Some("1234"));
    let sheet = spec(&["1234"]);
    assert_eq!(attribute("FASTQC (1234) #5", Some("FASTQC"), &sheet).as_deref(), Some("1234"));
    assert_eq!(attribute("FASTQC (5)", Some("FASTQC"), &open), None);
}

#[test]
fn a_snakemake_value_holding_a_paren_is_not_a_nextflow_key() {
    let open = spec(&[]);
    assert_eq!(
        attribute("trim:sample=A (x),lane=1", Some("trim"), &open).as_deref(),
        Some("A (x)")
    );
    assert_eq!(
        attribute("trim:lane=1,sample=B (2)", Some("trim"), &open).as_deref(),
        Some("B (2)")
    );
}

#[test]
fn with_no_sheet_a_file_name_tag_is_no_sample() {
    let open = spec(&[]);
    let gunzip = "NFCORE_RNASEQ:PREPARE_GENOME:GUNZIP_FASTA (genome.fa.gz) #1";
    assert_eq!(attribute(gunzip, Some("GUNZIP_FASTA"), &open), None);
    assert_eq!(attribute("GTF2BED (Genes.GTF) #1", Some("GTF2BED"), &open), None);
    let fastqc = "NFCORE_RNASEQ:RNASEQ:FASTQC (WT_REP1) #2";
    assert_eq!(attribute(fastqc, Some("FASTQC"), &open).as_deref(), Some("WT_REP1"));
    // A tag hint overrides it.
    let mut hinted = spec(&[]);
    hinted.hint.tag = Some(r"^(?P<sample>.+)\.fa\.gz$".into());
    assert_eq!(attribute(gunzip, Some("GUNZIP_FASTA"), &hinted).as_deref(), Some("genome"));
}

#[test]
fn a_tag_regex_hint_extracts_the_sample() {
    let mut s = spec(&[]);
    s.hint.tag = Some("^(?P<sample>[^_]+)_".into());
    assert_eq!(attribute("P (S1_L001) #1", Some("P"), &s).as_deref(), Some("S1"));
    assert_eq!(attribute("P (nounderscore) #2", Some("P"), &s), None);
    // A regex that does not compile names nothing.
    s.hint.tag = Some("(".into());
    assert_eq!(attribute("P (S1_L001) #1", Some("P"), &s), None);
    // With a sheet, the regex is trusted too: it names a sample the sheet does not list.
    let mut s = spec(&["S2"]);
    s.hint.tag = Some("^(?P<sample>[^_]+)_".into());
    assert_eq!(attribute("P (S1_L001) #1", Some("P"), &s).as_deref(), Some("S1"));
}

#[test]
fn a_sheet_id_with_a_dot_is_found_inside_a_tag() {
    let known = spec(&["NA12878.1", "NA12891"]);
    assert_eq!(
        attribute("MARKDUP (NA12878.1.markdup) #1", Some("MARKDUP"), &known).as_deref(),
        Some("NA12878.1")
    );
    // Only where it stands alone: `NA12878.10` is another sample.
    assert_eq!(attribute("MARKDUP (NA12878.10) #2", Some("MARKDUP"), &known), None);
    // The dotted id wins over an id it contains.
    let both = spec(&["NA12878", "NA12878.1"]);
    assert_eq!(
        attribute("MARKDUP (NA12878.1.markdup) #1", Some("MARKDUP"), &both).as_deref(),
        Some("NA12878.1")
    );
}

#[test]
fn a_step_forced_to_the_study_names_no_sample() {
    let mut s = spec(&[]);
    s.hint.steps.insert("joint".into(), StepScope::Study);
    assert_eq!(attribute("joint:sample=A", Some("joint"), &s), None);
}

#[test]
fn hints_name_a_nextflow_step_by_its_full_or_short_name() {
    let mut s = spec(&[]);
    s.hint.steps.insert("STAR_ALIGN".into(), StepScope::Study);
    let key = "NFCORE_RNASEQ:RNASEQ:STAR_ALIGN (WT_REP1) #1";
    let group = "NFCORE_RNASEQ:RNASEQ:STAR_ALIGN";
    assert_eq!(attribute(key, Some(group), &s), None);
    // The full name wins over the short one.
    s.hint.steps.insert(group.into(), StepScope::Sample);
    assert_eq!(attribute(key, Some(group), &s).as_deref(), Some("WT_REP1"));
    // An unknown scope forces nothing.
    let mut u = spec(&[]);
    u.hint.steps.insert("STAR_ALIGN".into(), StepScope::Unknown("lane".into()));
    assert_eq!(attribute(key, Some(group), &u).as_deref(), Some("WT_REP1"));
}

#[test]
fn an_unknown_step_scope_round_trips_and_keeps_the_anchor() {
    let hint: SampleHint = serde_json::from_value(serde_json::json!({
        "steps": {"x": "lane", "y": "study"}
    }))
    .unwrap();
    assert_eq!(hint.steps["x"], StepScope::Unknown("lane".into()));
    assert_eq!(hint.steps["y"], StepScope::Study);
    assert_eq!(
        serde_json::to_value(&hint).unwrap(),
        serde_json::json!({"steps": {"x": "lane", "y": "study"}})
    );

    let stored = serde_json::json!({
        "kind": "run_submitted", "workflow_version_id": "wf1", "git_sha": "abc",
        "image_digest": "", "params": {}, "input_uris": [],
        "samples": {"ids": ["A"], "hint": {"steps": {"x": "lane"}}}
    });
    let data: crate::event::EventData = serde_json::from_value(stored.clone()).unwrap();
    let crate::event::EventData::RunSubmitted(anchor) = &data else {
        panic!("anchor degraded to {data:?}");
    };
    assert_eq!(anchor.samples.as_ref().unwrap().hint.steps["x"], StepScope::Unknown("lane".into()));
    assert_eq!(serde_json::to_value(&data).unwrap(), stored);
}

fn fact(sample: Option<&str>, group: &str, state: &str) -> TaskFact {
    TaskFact {
        sample: sample.map(str::to_string),
        group: Some(group.to_string()),
        state: state.to_string(),
        cached: false,
        attempts: 1,
        outputs: false,
    }
}

fn dag(nodes: &[&str], edges: &[(&str, &str)]) -> (BTreeSet<String>, BTreeSet<(String, String)>) {
    (
        nodes.iter().map(std::string::ToString::to_string).collect(),
        edges.iter().map(|(a, b)| (a.to_string(), b.to_string())).collect(),
    )
}

fn states(grid: &SampleGrid) -> Vec<(&str, SampleState)> {
    grid.samples.iter().map(|r| (r.id.as_str(), r.state)).collect()
}

#[test]
fn the_grid_orders_steps_by_the_dag_and_rolls_up_samples() {
    let (n, e) = dag(&["trim", "align", "multiqc"], &[("trim", "align"), ("align", "multiqc")]);
    let tasks = vec![
        fact(Some("A"), "trim", "succeeded"),
        fact(Some("A"), "align", "succeeded"),
        fact(Some("B"), "trim", "succeeded"),
        fact(Some("B"), "align", "failed"),
        fact(None, "multiqc", "queued"),
    ];
    let grid = SampleGrid::build(&spec(&["A", "B", "C"]), &tasks, (&n, &e), RunEnd::Live);
    let order: Vec<_> = grid.steps.iter().map(|s| s.group.as_str()).collect();
    assert_eq!(order, ["trim", "align", "multiqc"]);
    assert_eq!(grid.steps[2].scope, StepScope::Study);
    assert_eq!(
        states(&grid),
        [("A", SampleState::Done), ("B", SampleState::Failed), ("C", SampleState::Waiting)]
    );
    assert_eq!(grid.study["multiqc"].state, CellState::Waiting);
    assert_eq!(grid.counts(), SampleCounts { total: 3, done: 1, failed: 1, declared: true });
}

#[test]
fn an_edge_from_outside_the_dag_does_not_hold_its_target_back() {
    // `ghost` is no node: its edge must not keep `zeta` (and so `alpha`) out of topological order,
    // which would leave them for the cycle fallback's name order (alpha, zeta).
    let (n, e) = dag(&["zeta", "alpha"], &[("ghost", "zeta"), ("zeta", "alpha")]);
    let tasks = vec![fact(Some("A"), "zeta", "succeeded"), fact(Some("A"), "alpha", "running")];
    let grid = SampleGrid::build(&spec(&["A"]), &tasks, (&n, &e), RunEnd::Live);
    let order: Vec<_> = grid.steps.iter().map(|s| s.group.as_str()).collect();
    assert_eq!(order, ["zeta", "alpha"]);
}

#[test]
fn a_cycle_in_the_dag_keeps_its_steps_in_name_order() {
    let (n, e) = dag(
        &["start", "zeta", "alpha"],
        &[("start", "zeta"), ("zeta", "alpha"), ("alpha", "zeta")],
    );
    let tasks = vec![
        fact(Some("A"), "start", "succeeded"),
        fact(Some("A"), "zeta", "running"),
        fact(Some("A"), "alpha", "queued"),
    ];
    let grid = SampleGrid::build(&spec(&["A"]), &tasks, (&n, &e), RunEnd::Live);
    let order: Vec<_> = grid.steps.iter().map(|s| s.group.as_str()).collect();
    assert_eq!(order, ["start", "alpha", "zeta"]);
}

#[test]
fn the_grid_keeps_the_sheet_s_order() {
    let (n, e) = dag(&[], &[]);
    let tasks = vec![
        fact(Some("P1"), "trim", "running"),
        fact(Some("P10"), "trim", "running"),
        fact(Some("P2"), "trim", "running"),
        fact(Some("X"), "trim", "running"),
    ];
    let grid = SampleGrid::build(&spec(&["P2", "P10", "P1"]), &tasks, (&n, &e), RunEnd::Live);
    let ids: Vec<_> = grid.samples.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["P2", "P10", "P1", "X"]);
}

#[test]
fn a_sample_is_not_done_while_a_step_has_yet_to_start() {
    let (n, e) = dag(&["trim", "align"], &[("trim", "align")]);
    let tasks = vec![fact(Some("A"), "trim", "succeeded")];
    let running = SampleGrid::build(&spec(&["A"]), &tasks, (&n, &e), RunEnd::Live);
    assert_eq!(running.samples[0].state, SampleState::Running);
    // Once the run has succeeded there is nothing more to come.
    let ended = SampleGrid::build(&spec(&["A"]), &tasks, (&n, &e), RunEnd::Succeeded);
    assert_eq!(ended.samples[0].state, SampleState::Done);
}

#[test]
fn without_a_dag_a_sample_settles_only_when_the_run_succeeds() {
    // Nextflow reports its DAG only at the end: a live run has none.
    let (n, e) = dag(&[], &[]);
    let tasks = vec![fact(Some("A"), "FASTQC", "succeeded")];
    let live = SampleGrid::build(&spec(&[]), &tasks, (&n, &e), RunEnd::Live);
    assert_eq!(live.samples[0].state, SampleState::Running);
    let done = SampleGrid::build(&spec(&[]), &tasks, (&n, &e), RunEnd::Succeeded);
    assert_eq!(done.samples[0].state, SampleState::Done);
}

#[test]
fn a_run_that_stops_stops_its_unfinished_samples() {
    let (n, e) = dag(&["trim", "align"], &[("trim", "align")]);
    let tasks = vec![
        fact(Some("A"), "trim", "succeeded"),
        fact(Some("A"), "align", "succeeded"),
        fact(Some("B"), "trim", "succeeded"),
        fact(Some("C"), "trim", "failed"),
    ];
    let sheet = spec(&["A", "B", "C", "D"]);
    let stopped = SampleGrid::build(&sheet, &tasks, (&n, &e), RunEnd::Stopped);
    assert_eq!(
        states(&stopped),
        [
            ("A", SampleState::Done),
            ("B", SampleState::Stopped),
            ("C", SampleState::Failed),
            ("D", SampleState::Stopped)
        ]
    );
    assert_eq!(stopped.counts(), SampleCounts { total: 4, done: 1, failed: 1, declared: true });
    // A succeeded run's sheet sample that no task named never ran.
    let ok = vec![fact(Some("A"), "trim", "succeeded")];
    let grid = SampleGrid::build(&spec(&["A", "D"]), &ok, (&n, &e), RunEnd::Succeeded);
    assert_eq!(states(&grid), [("A", SampleState::Done), ("D", SampleState::Stopped)]);
}

#[test]
fn in_a_workflow_whose_samples_take_different_paths_a_sample_is_done_when_the_run_succeeds() {
    let (n, e) = dag(&["fast", "slow"], &[]);
    let tasks = vec![fact(Some("A"), "fast", "succeeded"), fact(Some("B"), "slow", "succeeded")];
    let running = SampleGrid::build(&spec(&[]), &tasks, (&n, &e), RunEnd::Live);
    assert_eq!(running.counts().done, 0);
    let ended = SampleGrid::build(&spec(&[]), &tasks, (&n, &e), RunEnd::Succeeded);
    assert_eq!(ended.counts().done, 2);
    // A sample with an unfinished task is still not done.
    let mut stuck = tasks.clone();
    stuck.push(fact(Some("B"), "slow", "queued"));
    let ended = SampleGrid::build(&spec(&[]), &stuck, (&n, &e), RunEnd::Succeeded);
    assert_eq!(ended.counts().done, 1);
}

#[test]
fn no_samples_show_until_a_task_names_one() {
    let (n, e) = dag(&[], &[]);
    let tasks = vec![fact(None, "hello", "succeeded")];
    let grid = SampleGrid::build(&spec(&["S1", "S2"]), &tasks, (&n, &e), RunEnd::Succeeded);
    assert!(grid.samples.is_empty());
    assert_eq!(grid.steps[0].scope, StepScope::Study);
    let none_yet = SampleGrid::build(&spec(&["S1"]), &[], (&n, &e), RunEnd::Live);
    assert!(none_yet.samples.is_empty());
}

#[test]
fn a_step_forced_to_sample_is_per_sample_even_with_no_attributed_task() {
    let (n, e) = dag(&[], &[]);
    let mut s = spec(&[]);
    s.hint.steps.insert("prep".into(), StepScope::Sample);
    s.hint.steps.insert("NFCORE:JOINT".into(), StepScope::Study);
    let tasks = vec![
        fact(Some("A"), "align", "running"),
        fact(None, "prep", "running"),
        fact(None, "prep", "queued"),
        fact(Some("A"), "NFCORE:JOINT", "running"),
    ];
    let grid = SampleGrid::build(&s, &tasks, (&n, &e), RunEnd::Live);
    let scopes: BTreeMap<_, _> =
        grid.steps.iter().map(|st| (st.group.as_str(), st.scope.clone())).collect();
    assert_eq!(scopes["prep"], StepScope::Sample);
    assert_eq!(scopes["NFCORE:JOINT"], StepScope::Study);
    assert_eq!(grid.unattributed, 2);
    assert_eq!(grid.study["NFCORE:JOINT"].tasks, 1);
    assert!(!grid.study.contains_key("prep"));
}

#[test]
fn cells_merge_tasks_and_count_retries_and_reuse() {
    let (n, e) = dag(&[], &[]);
    let mut lane1 = fact(Some("A"), "trim", "succeeded");
    lane1.cached = true;
    let mut lane2 = fact(Some("A"), "trim", "running");
    lane2.attempts = 2;
    let grid = SampleGrid::build(&spec(&[]), &[lane1.clone(), lane2], (&n, &e), RunEnd::Live);
    let cell = &grid.samples[0].cells["trim"];
    assert_eq!((cell.state, cell.attempts, cell.tasks), (CellState::Running, 3, 2));
    assert!(!grid.declared);
    let reused = SampleGrid::build(&spec(&[]), &[lane1], (&n, &e), RunEnd::Succeeded);
    assert_eq!(reused.samples[0].cells["trim"].state, CellState::Reused);
    assert_eq!(reused.samples[0].state, SampleState::Done);
}

#[test]
fn an_unknown_task_state_reads_as_waiting() {
    assert_eq!(CellState::of("paused", false), CellState::Waiting);
    assert_eq!(CellState::of("succeeded", true), CellState::Reused);
}

#[test]
fn an_unmatched_task_in_a_per_sample_step_is_counted_not_placed() {
    let (n, e) = dag(&[], &[]);
    let tasks = vec![fact(Some("A"), "align", "running"), fact(None, "align", "running")];
    let grid = SampleGrid::build(&spec(&[]), &tasks, (&n, &e), RunEnd::Live);
    assert_eq!(grid.unattributed, 1);
    assert!(grid.study.is_empty());
}

#[test]
fn labels_come_from_the_hint_else_the_process_name() {
    let (n, e) = dag(&[], &[]);
    let mut s = spec(&[]);
    s.hint.labels.insert("STAR_ALIGN".into(), "Align (STAR)".into());
    let tasks = vec![
        fact(Some("A"), "NFCORE:RNASEQ:STAR_ALIGN", "running"),
        fact(Some("A"), "NFCORE:RNASEQ:TRIM_GALORE", "running"),
    ];
    let grid = SampleGrid::build(&s, &tasks, (&n, &e), RunEnd::Live);
    let labels: Vec<_> = grid.steps.iter().map(|s| s.label.as_str()).collect();
    assert_eq!(labels, ["Align (STAR)", "TRIM GALORE"]);
}

#[test]
fn sample_ids_are_the_sheet_s_in_order_then_the_others_sorted() {
    let sheet: Vec<String> = ["S2", "S1", "S2", "S10"].map(String::from).to_vec();
    assert_eq!(sample_ids(&sheet, ["S9", "S1", "A", "S9"]), ["S2", "S1", "S10", "A", "S9"]);
    assert_eq!(sample_ids(&[], ["b", "a"]), ["a", "b"]);
    assert!(sample_ids(&[], []).is_empty());
}

// ── a resumed run's reused samples ──

/// The origin run of the resume tests: A and C done, B failed at `align` after `trim`.
fn origin_grid() -> SampleGrid {
    let (n, e) = dag(&["trim", "align"], &[("trim", "align")]);
    let tasks = vec![
        fact(Some("A"), "trim", "succeeded"),
        fact(Some("A"), "align", "succeeded"),
        fact(Some("B"), "trim", "succeeded"),
        fact(Some("B"), "align", "failed"),
        fact(Some("C"), "trim", "succeeded"),
        fact(Some("C"), "align", "succeeded"),
    ];
    SampleGrid::build(&spec(&["A", "B", "C"]), &tasks, (&n, &e), RunEnd::Stopped)
}

fn resumed_spec() -> SampleSpec {
    SampleSpec { reused: origin_grid().reused_by_resume(), ..spec(&["A", "B", "C"]) }
}

#[test]
fn a_grid_carries_its_finished_cells_to_a_resume() {
    let one = |n: &[(&str, i64)]| n.iter().map(|(g, c)| (g.to_string(), *c)).collect();
    assert_eq!(
        origin_grid().reused_by_resume(),
        [
            ReusedSample { id: "A".into(), done: true, steps: one(&[("align", 1), ("trim", 1)]) },
            ReusedSample { id: "B".into(), done: false, steps: one(&[("trim", 1)]) },
            ReusedSample { id: "C".into(), done: true, steps: one(&[("align", 1), ("trim", 1)]) },
        ]
    );
}

#[test]
fn a_snakemake_resume_shows_the_samples_it_skipped_as_reused() {
    // Snakemake's resume creates tasks only for what is left: B's `align` (and its local
    // target rule `all`). A and C have no task at all.
    let (n, e) = dag(&["align", "all"], &[("align", "all")]);
    let mut all = fact(None, "all", "succeeded");
    all.attempts = 0;
    let tasks = vec![fact(Some("B"), "align", "succeeded"), all];
    let grid = SampleGrid::build(&resumed_spec(), &tasks, (&n, &e), RunEnd::Succeeded);
    assert_eq!(
        states(&grid),
        [("A", SampleState::Done), ("B", SampleState::Done), ("C", SampleState::Done)]
    );
    let cell = |id: &str, g: &str| grid.samples.iter().find(|r| r.id == id).unwrap().cells[g].state;
    assert_eq!(cell("A", "trim"), CellState::Reused);
    assert_eq!(cell("A", "align"), CellState::Reused);
    assert_eq!(cell("B", "trim"), CellState::Reused);
    assert_eq!(cell("B", "align"), CellState::Done);
    let steps: Vec<&str> = grid.steps.iter().map(|s| s.group.as_str()).collect();
    assert_eq!(steps, ["align", "trim"]);
    assert!(grid.study.is_empty(), "{:?}", grid.study);
    assert_eq!(grid.counts(), SampleCounts { total: 3, done: 3, failed: 0, declared: true });
    // While it runs, the samples finished before are done already; B is still going.
    let live = vec![fact(Some("B"), "align", "running")];
    let grid = SampleGrid::build(&resumed_spec(), &live, (&n, &e), RunEnd::Live);
    assert_eq!(
        states(&grid),
        [("A", SampleState::Done), ("B", SampleState::Running), ("C", SampleState::Done)]
    );
    // Right after the launch, before B has a task of its own here: B is waiting, not running (all
    // it has are the cells carried from the earlier run).
    let grid = SampleGrid::build(&resumed_spec(), &[], (&n, &e), RunEnd::Live);
    assert_eq!(
        states(&grid),
        [("A", SampleState::Done), ("B", SampleState::Waiting), ("C", SampleState::Done)]
    );
    let grid = SampleGrid::build(&resumed_spec(), &[], (&n, &e), RunEnd::Stopped);
    assert_eq!(states(&grid)[1], ("B", SampleState::Stopped));
    // A sample dropped from the resumed run's sheet is not brought back.
    let narrower = SampleSpec { reused: origin_grid().reused_by_resume(), ..spec(&["B"]) };
    let grid = SampleGrid::build(&narrower, &tasks, (&n, &e), RunEnd::Succeeded);
    assert_eq!(states(&grid), [("B", SampleState::Done)]);
}

#[test]
fn a_nextflow_resume_reports_its_cached_tasks_and_nothing_counts_twice() {
    // Nextflow's resume reports every task, the cached ones as cached: the carried cells add
    // nothing, so the grid is the one the run's own tasks give.
    let (n, e) = dag(&["trim", "align"], &[("trim", "align")]);
    let cached =
        |s: &str, g: &str| TaskFact { cached: true, attempts: 0, ..fact(Some(s), g, "succeeded") };
    let tasks = vec![
        cached("A", "trim"),
        cached("A", "align"),
        cached("B", "trim"),
        fact(Some("B"), "align", "succeeded"),
        cached("C", "trim"),
        cached("C", "align"),
    ];
    let carried = SampleGrid::build(&resumed_spec(), &tasks, (&n, &e), RunEnd::Succeeded);
    let own = SampleGrid::build(&spec(&["A", "B", "C"]), &tasks, (&n, &e), RunEnd::Succeeded);
    assert_eq!(carried, own);
    assert_eq!(carried.samples[0].cells["trim"].tasks, 1);
    assert_eq!(carried.counts().done, 3);
}

// ── the workflow's target rule ──

#[test]
fn the_local_target_rule_is_no_step() {
    let (n, e) = dag(&["trim", "all"], &[("trim", "all")]);
    let target = |f: fn(&mut TaskFact)| {
        let mut all = fact(None, "all", "succeeded");
        all.attempts = 0;
        f(&mut all);
        vec![fact(Some("A"), "trim", "succeeded"), all]
    };
    let shown = |tasks: &[TaskFact], dag: (&BTreeSet<String>, &BTreeSet<(String, String)>)| {
        let grid = SampleGrid::build(&spec(&[]), tasks, dag, RunEnd::Succeeded);
        let steps: Vec<String> = grid.steps.iter().map(|s| s.group.clone()).collect();
        (steps, grid.study.contains_key("all"))
    };
    let hidden = (vec!["trim".to_string()], false);
    let visible = (vec!["trim".to_string(), "all".to_string()], true);
    assert_eq!(shown(&target(|_| {}), (&n, &e)), hidden);
    // Still waiting to be reached: hidden all the same.
    assert_eq!(shown(&target(|t| t.state = "queued".into()), (&n, &e)), hidden);
    // A local final step that wrote something is work, not the target.
    assert_eq!(shown(&target(|t| t.outputs = true), (&n, &e)), visible);
    // A failure always shows.
    assert_eq!(shown(&target(|t| t.state = "failed".into()), (&n, &e)), visible);
    // A Batch job (an attempt) is work.
    assert_eq!(shown(&target(|t| t.attempts = 1), (&n, &e)), visible);
    // Not a sink (something consumes it), or no DAG to tell: shown.
    let (n2, e2) = dag(&["trim", "all", "x"], &[("trim", "all"), ("all", "x")]);
    assert!(shown(&target(|_| {}), (&n2, &e2)).1);
    let (none_n, none_e) = dag(&[], &[]);
    assert!(shown(&target(|_| {}), (&none_n, &none_e)).1);
}
