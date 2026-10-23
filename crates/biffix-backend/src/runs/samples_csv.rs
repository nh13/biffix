//! An experiment's samples as a CSV file for a spreadsheet ([`samples_csv_body`]): one row per
//! sample per run, read within limits on runs, size and time, past which the answer is a 413 whose
//! message says which limit and what to do.

use std::time::Duration;

use tokio::time::Instant;

use biffix_api::RunSummary;
use biffix_core::ids::RunId;

use crate::error::ServiceError;
use crate::store::experiments::ExperimentReads;
use crate::store::StoreError;

/// The header of `GET /experiments/{id}/samples.csv`, one column per [`sample_csv_record`] field, in
/// plain words: the file is for opening in a spreadsheet.
const SAMPLES_CSV_HEADER: [&str; 12] = [
    "Sample",
    "Sample status",
    "Reused",
    "Analysis",
    "Run ID",
    "Analysis status",
    "Pipeline",
    "Pipeline version",
    "Engine",
    "Submitted by",
    "Submitted (UTC)",
    "Finished (UTC)",
];

/// How much one samples download may read and return. Past any limit the answer is a 413 that says
/// which and what to do, never a file cut short (a spreadsheet cannot say it is incomplete).
#[derive(Clone, Copy, Debug)]
pub(crate) struct SamplesCsvLimits {
    /// The most runs in the experiment.
    pub(crate) runs: i64,
    /// The most bytes the body may take once JSON-escaped ([`crate::csv::json_escaped_len`]).
    pub(crate) bytes: usize,
    /// The most time reading may take, from the request's start.
    pub(crate) time: Duration,
}

impl SamplesCsvLimits {
    /// The deployed limits, stated in the doc of `GET /experiments/{id}/samples.csv` (keep them in
    /// step).
    ///
    /// - `bytes`: a synchronous Lambda response is at most 6 MB (as `fastqc.rs` and `events.rs`
    ///   budget for), and the Lambda sends a `text/*` body as a JSON string inside its response
    ///   envelope (`crate::api::lambda::LambdaReply`, the byte-order mark included), so the escaped
    ///   size is what counts (a CRLF is 4 bytes there). 5 MiB (5,242,880
    ///   bytes) of it leaves over 700 KB of 6 MB, read conservatively as 6,000,000 bytes, for the
    ///   envelope (status, headers) and rounding. A typical row is
    ///   ~150-200 bytes (+2 for its escaped CRLF), so ~25-35k sample rows fit.
    /// - `time`: the API Lambda times out at 29 s ([`crate::config::API_LAMBDA_TIMEOUT`]); 20 s of
    ///   reading ([`crate::config::REQUEST_WAIT_BUDGET`]) leaves the rest for building and sending up
    ///   to 5 MiB, and a read still running at the deadline is abandoned rather than waited for.
    /// - `runs`: 1,000 runs is five run pages and five sample-fact reads of 200 runs each (the
    ///   detail's one read is one such chunk), a bound on the work before the byte limit can speak.
    pub(crate) const DEPLOYED: Self =
        Self { runs: 1_000, bytes: 5 * 1024 * 1024, time: crate::config::REQUEST_WAIT_BUDGET };
}

/// What a 413 from the samples download tells its reader to do.
const SAMPLES_CSV_ADVICE: &str = "Split the experiment into smaller ones (move some of its runs \
into another experiment) and download the samples of each.";

/// A 413 for a samples download over one of its [`SamplesCsvLimits`].
fn samples_csv_too_large(why: String) -> ServiceError {
    ServiceError::TooLarge(format!("{why} {SAMPLES_CSV_ADVICE}"))
}

/// `bytes` in words: whole MiB when it is a whole number of them, else bytes.
fn size_words(bytes: usize) -> String {
    const MIB: usize = 1024 * 1024;
    if bytes >= MIB && bytes.is_multiple_of(MIB) {
        format!("{} MiB", bytes / MIB)
    } else {
        format!("{bytes} bytes")
    }
}

/// `read`'s result, or `too_slow()` when `deadline` has passed before or while it runs (the read is
/// dropped, so a slow query is not waited for).
async fn by_deadline<T, F>(
    deadline: Instant,
    read: F,
    too_slow: impl Fn() -> ServiceError,
) -> Result<T, ServiceError>
where
    F: std::future::Future<Output = Result<T, StoreError>>,
{
    if Instant::now() >= deadline {
        return Err(too_slow());
    }
    match tokio::time::timeout_at(deadline, read).await {
        Ok(result) => result.map_err(ServiceError::from),
        Err(_) => Err(too_slow()),
    }
}

/// The samples CSV as it is built: the body, and its size as the Lambda will send it.
struct SamplesCsv {
    body: String,
    escaped_len: usize,
    /// Data rows written so far.
    rows: usize,
    limit: usize,
}

impl SamplesCsv {
    /// The byte-order mark and the header, against a size `limit`.
    fn new(limit: usize) -> Self {
        let mut csv = Self { body: String::new(), escaped_len: 0, rows: 0, limit };
        csv.append(crate::csv::UTF8_BOM);
        csv.append(&crate::csv::record(&SAMPLES_CSV_HEADER));
        csv
    }

    fn append(&mut self, text: &str) {
        self.escaped_len += crate::csv::json_escaped_len(text);
        self.body.push_str(text);
    }

    /// Add a data row; `false` once the body is over the size limit.
    fn push_row<S: AsRef<str>>(&mut self, fields: &[S]) -> bool {
        self.append(&crate::csv::record(fields));
        self.rows += 1;
        self.escaped_len <= self.limit
    }
}

/// A state's wire name in plain words, as the UI shows it: `preparing_images` is `Preparing images`,
/// `done` is `Done` (an unknown state reads the same way).
fn state_words(state: &str) -> String {
    let words = state.replace('_', " ");
    let mut chars = words.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => words,
    }
}

/// Epoch seconds as `YYYY-MM-DD HH:MM:SS` (UTC), the form a spreadsheet reads as a date and time;
/// empty when absent.
fn csv_time(secs: Option<f64>) -> String {
    secs.map(|s| {
        let micros = biffix_core::time::Timestamp::from_epoch_seconds(s).as_micros();
        biffix_core::time::utc_datetime(micros, ' ')
    })
    .unwrap_or_default()
}

/// Where the person downloading is, a time zone (the UI sends its own), and when: what the
/// analysis names in a samples CSV ([`analysis_name`]) read their start time in. Each time is read
/// with the zone's rules for that instant (daylight saving included), as the browser reads it.
#[derive(Clone, Debug)]
pub(crate) struct NameZone {
    zone: jiff::tz::TimeZone,
    /// The current year in `zone`: a start in it is named without its year.
    this_year: i64,
}

impl NameZone {
    /// The zone an IANA `name` names (`Europe/Berlin`); UTC when none is given (or it is blank).
    /// `Err` is the 400 for a name not in the time-zone database.
    pub(crate) fn named(name: Option<&str>, now_micros: i64) -> Result<Self, String> {
        let zone = match name.map(str::trim).filter(|n| !n.is_empty()) {
            None => jiff::tz::TimeZone::UTC,
            Some(name) => jiff::tz::TimeZone::get(name).map_err(|_| {
                format!("unknown time zone `{name}` (an IANA name, such as `Europe/Berlin`)")
            })?,
        };
        let mut named = NameZone { zone, this_year: 0 };
        named.this_year = named.civil(now_micros).0;
        Ok(named)
    }

    /// `(year, month, day, hour, minute)` of `micros`, in this zone.
    fn civil(&self, micros: i64) -> (i64, u32, u32, i64, i64) {
        let at = jiff::Timestamp::from_microsecond(micros).unwrap_or(jiff::Timestamp::UNIX_EPOCH);
        let t = self.zone.to_datetime(at);
        (
            i64::from(t.year()),
            u32::try_from(t.month()).unwrap_or(1),
            u32::try_from(t.day()).unwrap_or(1),
            i64::from(t.hour()),
            i64::from(t.minute()),
        )
    }
}

/// The analysis's name as the UI shows it (`ui/src/lib/runs/runName.ts`, `analysisName`): its own
/// name, else one derived from its pipeline (title, else registry name), when it started
/// (`Oct 1 14:05`, with the year when not this year's, in `zone`) and its sample count
/// (`3 samples`), leaving out what is not known; else (nothing known) its id. Two differences
/// from the UI: the start is shown at the one offset `zone` holds (the downloader's offset now),
/// where the UI shows each start at its own date's offset (so a start across a DST change reads
/// an hour apart), and the count is grouped as in `en-US` where the UI groups it in the
/// browser's locale.
pub(crate) fn analysis_name(run: &RunSummary, zone: &NameZone) -> String {
    if let Some(name) = run.run_name.as_deref().map(js_trim).filter(|n| !n.is_empty()) {
        return name.to_string();
    }
    let pipeline = [run.workflow_title.as_deref(), run.workflow_name.as_deref()]
        .into_iter()
        .flatten()
        .map(js_trim)
        .find(|t| !t.is_empty());
    let started = run.submitted_at.map(|s| {
        const MONTHS: [&str; 12] =
            ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
        let micros = biffix_core::time::Timestamp::from_epoch_seconds(s).as_micros();
        let (y, m, d, hh, mm) = zone.civil(micros);
        let month = MONTHS[(m as usize).saturating_sub(1).min(11)];
        let day =
            if y == zone.this_year { format!("{month} {d}") } else { format!("{month} {d}, {y}") };
        format!("{day} {hh:02}:{mm:02}")
    });
    if pipeline.is_none() && started.is_none() {
        return run.run_id.to_string();
    }
    let samples = run.samples.as_ref().map_or(0, |s| s.total);
    let samples = (samples > 0).then(|| {
        let n = thousands(samples);
        if samples == 1 {
            format!("{n} sample")
        } else {
            format!("{n} samples")
        }
    });
    [pipeline.map(str::to_string), started, samples]
        .into_iter()
        .flatten()
        .collect::<Vec<_>>()
        .join(" · ")
}

/// `n` with `,` between groups of three digits, as `toLocaleString('en-US')` groups it.
fn thousands(n: i64) -> String {
    let digits = n.unsigned_abs().to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    if n < 0 {
        format!("-{out}")
    } else {
        out
    }
}

/// `s` trimmed as JavaScript's `String.prototype.trim` trims it ([`js_trimmed`]).
fn js_trim(s: &str) -> &str {
    s.trim_matches(js_trimmed)
}

/// One CSV row: sample `row`, then its run's columns ([`run_csv_columns`]). `Reused` is `yes` only
/// when the sample finished in that run (done) and every one of its steps there was satisfied from
/// an earlier run's results (an engine cache hit, or carried from the run it resumed); a sample
/// still going or stopped reads `no`, as its later steps were never satisfied. Statuses are in
/// words ([`state_words`]).
fn sample_csv_record(
    row: &biffix_core::sample_progress::SampleRow,
    run_columns: &[String; 9],
) -> Vec<String> {
    use biffix_core::sample_progress::{CellState, SampleState};
    let reused = row.state == SampleState::Done
        && !row.cells.is_empty()
        && row.cells.values().all(|c| c.state == CellState::Reused);
    let mut record = Vec::with_capacity(3 + run_columns.len());
    record.extend([
        row.id.clone(),
        state_words(row.state.as_str()),
        if reused { "yes" } else { "no" }.to_string(),
    ]);
    record.extend_from_slice(run_columns);
    record
}

/// The CSV columns of `run` every one of its sample rows repeats; the analysis is named as the UI
/// names it ([`analysis_name`], its start time in `zone`).
fn run_csv_columns(run: &RunSummary, zone: &NameZone) -> [String; 9] {
    [
        analysis_name(run, zone),
        run.run_id.to_string(),
        state_words(&run.state),
        run.workflow_name.clone().unwrap_or_default(),
        run.workflow.as_ref().map(ToString::to_string).unwrap_or_default(),
        run.engine.clone().unwrap_or_default(),
        run.submitter_label.clone().or_else(|| run.submitter.clone()).unwrap_or_default(),
        csv_time(run.submitted_at),
        csv_time(run.completed_at),
    ]
}

/// Whether `c` is whitespace JavaScript's `String.prototype.trim` removes: Unicode white space
/// (but not U+0085, which JavaScript leaves) and the byte-order mark U+FEFF.
fn js_trimmed(c: char) -> bool {
    (c.is_whitespace() && c != '\u{85}') || c == '\u{feff}'
}

/// The download's file name: `<experiment name>-samples.csv`, the name trimmed and each character a
/// file name cannot hold on some system (`/ \ : * ? " < > |` and control characters) made `-`;
/// `experiment` when nothing is left. The UI saves the file under the name `samplesCsvFilename`
/// (`ui/src/lib/experiments/experiments.ts`) gives by the same rule, as it cannot read this header
/// cross-origin; both test the same cases, so a direct download and the button agree.
pub(crate) fn samples_csv_filename(name: &str) -> String {
    let safe: String = name
        .trim_matches(js_trimmed)
        .chars()
        .map(|c| if c.is_control() || "/\\:*?\"<>|".contains(c) { '-' } else { c })
        .collect();
    let safe = if safe.is_empty() { "experiment" } else { &safe };
    format!("{safe}-samples.csv")
}

/// The CSV body of experiment `id`'s samples (`GET /experiments/{id}/samples.csv`): `runs_total`
/// runs (its count from [`ExperimentReads::experiment_run_stats`]) read within `limits` from
/// `started`, or the 413 for the limit it passed.
pub(crate) async fn samples_csv_body(
    store: &dyn ExperimentReads,
    id: &str,
    runs_total: i64,
    limits: SamplesCsvLimits,
    started: Instant,
    zone: NameZone,
) -> Result<String, ServiceError> {
    if runs_total > limits.runs {
        return Err(samples_csv_too_large(format!(
            "This experiment has {runs_total} runs (analyses), more than the {} one samples \
             download can cover.",
            limits.runs
        )));
    }
    let deadline = started + limits.time;
    let too_slow = || {
        samples_csv_too_large(format!(
            "Reading the samples of this experiment's {runs_total} runs (analyses) took longer than \
             the {} s one download may take.",
            limits.time.as_secs()
        ))
    };
    let mut runs = by_deadline(deadline, store.all_experiment_runs(id), too_slow).await?;
    runs.reverse();
    let mut csv = SamplesCsv::new(limits.bytes);
    let mut runs_read = 0;
    for chunk in runs.chunks(crate::store::runs::MAX_RUN_PAGE as usize) {
        let ids: Vec<RunId> = chunk.iter().map(|r| r.run_id.clone()).collect();
        let facts = by_deadline(deadline, store.runs_sample_facts(&ids), too_slow).await?;
        for run in chunk {
            runs_read += 1;
            let Some(facts) = facts.get(run.run_id.as_str()) else {
                continue;
            };
            let grid = crate::runs::detail::grid_from_facts(facts, &run.state);
            let run_columns = run_csv_columns(run, &zone);
            for row in &grid.samples {
                if !csv.push_row(&sample_csv_record(row, &run_columns)) {
                    return Err(samples_csv_too_large(format!(
                        "This experiment's samples are too many to download at once: the file \
                         would be larger than {}, the most one download can hold (its oldest \
                         {runs_read} of {} runs (analyses) already give {} sample rows).",
                        size_words(limits.bytes),
                        runs.len(),
                        csv.rows
                    )));
                }
            }
        }
    }
    Ok(csv.body)
}

#[cfg(test)]
mod tests {

    use super::*;

    #[test]
    fn an_unnamed_analysis_is_named_as_the_ui_names_it() {
        let run = |v: serde_json::Value| -> RunSummary {
            let mut base = serde_json::json!({
                "run_id": "run-1", "state": "succeeded", "tasks_observed": 0,
                "tasks_succeeded": 0, "tasks_running": 0, "tasks_failed": 0, "tasks_cached": 0,
            });
            base.as_object_mut().unwrap().extend(v.as_object().unwrap().clone());
            serde_json::from_value(base).unwrap()
        };
        // 2025-10-01 14:05:00 UTC; "now" in 2025, then in 2026.
        let at = 1_759_327_500.0;
        let now_2025 = 1_760_000_000_000_000;
        let utc = &NameZone::named(None, now_2025).unwrap();
        let samples = serde_json::json!({"total": 3, "done": 0, "failed": 0, "declared": true});
        assert_eq!(
            analysis_name(
                &run(serde_json::json!({"run_name": "  Liver  ", "submitted_at": at})),
                utc
            ),
            "Liver"
        );
        let unnamed = run(serde_json::json!({
            "workflow_name": "nf-core/rnaseq", "workflow_title": " Bulk RNA-seq ",
            "submitted_at": at, "samples": samples,
        }));
        assert_eq!(analysis_name(&unnamed, utc), "Bulk RNA-seq · Oct 1 14:05 · 3 samples");
        // In the downloader's zone (UTC-7), and with the year once it is not this year's.
        let phoenix = NameZone::named(Some("America/Phoenix"), now_2025).unwrap();
        assert_eq!(analysis_name(&unnamed, &phoenix), "Bulk RNA-seq · Oct 1 07:05 · 3 samples");
        let later = NameZone::named(Some(" "), now_2025 + 400 * 86_400_000_000).unwrap();
        assert_eq!(analysis_name(&unnamed, &later), "Bulk RNA-seq · Oct 1, 2025 14:05 · 3 samples");
        // Each start is read with its own instant's rules: a July run downloaded in December is
        // named in daylight time (UTC-4 in New York), as the UI names it, not December's UTC-5. A
        // run near midnight keeps its own day.
        let december = 1_765_800_000_000_000; // 2025-12-15
        let new_york = NameZone::named(Some("America/New_York"), december).unwrap();
        let july =
            run(serde_json::json!({"workflow_name": "rnaseq", "submitted_at": 1_751_393_100.0}));
        assert_eq!(analysis_name(&july, &new_york), "rnaseq · Jul 1 14:05");
        let late =
            run(serde_json::json!({"workflow_name": "rnaseq", "submitted_at": 1_751_342_400.0}));
        assert_eq!(analysis_name(&late, &new_york), "rnaseq · Jul 1 00:00");
        assert_eq!(
            NameZone::named(Some("Mars/Olympus"), december).unwrap_err(),
            "unknown time zone `Mars/Olympus` (an IANA name, such as `Europe/Berlin`)"
        );
        // The registry name without a title; nothing known: the id.
        assert_eq!(
            analysis_name(&run(serde_json::json!({"workflow_name": "rnaseq"})), utc),
            "rnaseq"
        );
        assert_eq!(analysis_name(&run(serde_json::json!({})), utc), "run-1");
        assert_eq!(thousands(1234567), "1,234,567");
        assert_eq!(thousands(999), "999");
    }

    #[test]
    fn the_samples_csv_stops_once_its_escaped_size_passes_the_limit() {
        let csv = SamplesCsv::new(usize::MAX);
        // The byte-order mark (3 bytes, not escaped) and the header, whose CRLF escapes to 4.
        let header = crate::csv::record(&SAMPLES_CSV_HEADER);
        assert_eq!(csv.escaped_len, 3 + header.len() + 2);
        let fixed = csv.escaped_len;
        // A row `a,"b""c"` CRLF: 10 bytes of CSV, 16 escaped (four quotes and the CRLF).
        let mut csv = SamplesCsv::new(fixed + 16);
        assert!(csv.push_row(&["a", "b\"c"]));
        assert_eq!(csv.escaped_len, fixed + 16);
        assert!(!csv.push_row(&["x"]));
        assert_eq!(csv.rows, 2);
    }

    #[tokio::test]
    async fn a_read_past_the_deadline_is_a_413() {
        let too_slow = || samples_csv_too_large("Too slow.".into());
        // A read still running at the deadline is dropped.
        let deadline = Instant::now() + Duration::from_millis(10);
        let pending = std::future::pending::<Result<(), StoreError>>();
        let e = by_deadline(deadline, pending, too_slow).await.unwrap_err();
        assert!(matches!(e, ServiceError::TooLarge(_)), "{e:?}");
        assert_eq!(e.to_string(), format!("Too slow. {SAMPLES_CSV_ADVICE}"));
        // Past it already: no read starts, even a ready one.
        let ready = std::future::ready(Ok::<_, StoreError>(1));
        assert!(by_deadline(Instant::now(), ready, too_slow).await.is_err());
        // In time: the read's own answer.
        let ready = std::future::ready(Ok::<_, StoreError>(1));
        let later = Instant::now() + Duration::from_secs(60);
        assert_eq!(by_deadline(later, ready, too_slow).await.unwrap(), 1);
    }

    /// A store holding experiment `x`'s `runs` runs, each naming two samples.
    fn experiment_of(runs: usize) -> crate::testing::MemStore {
        let store = crate::testing::MemStore::new();
        for i in 0..runs {
            let id = format!("r{i}");
            let mut d = crate::testing::run_detail(&id, "succeeded");
            d.run.experiment_id = Some("x".into());
            d.run.project = Some("demo".into());
            d.run.submitted_at = Some(1_700_000_000.0 + i as f64);
            d.dag_nodes = vec!["align".into()];
            for sample in ["S1", "S2"] {
                let mut t = crate::testing::task_view(
                    &format!("align:sample={sample}"),
                    "succeeded",
                    None,
                    None,
                );
                t.group = Some("align".into());
                d.tasks.push(t);
            }
            store.sample_specs.lock().unwrap().insert(
                id,
                biffix_core::event::SampleSpec {
                    ids: vec!["S1".into(), "S2".into()],
                    ..Default::default()
                },
            );
            store.insert(d);
        }
        store
    }

    fn zone() -> NameZone {
        NameZone::named(None, 1_760_000_000_000_000).unwrap()
    }

    #[tokio::test]
    async fn a_download_past_its_size_is_a_413_never_a_file_cut_short() {
        let store = experiment_of(3);
        // Room for the header, not a row: the first row passes it.
        let header = SamplesCsv::new(usize::MAX).escaped_len;
        let limits =
            SamplesCsvLimits { runs: 1_000, bytes: header + 1, time: Duration::from_secs(60) };
        let e = samples_csv_body(&store, "x", 3, limits, Instant::now(), zone()).await.unwrap_err();
        assert!(matches!(e, ServiceError::TooLarge(_)), "{e:?}");
        let message = e.to_string();
        assert_eq!(
            message,
            format!(
                "This experiment's samples are too many to download at once: the file would be \
                 larger than {} bytes, the most one download can hold (its oldest 1 of 3 runs \
                 (analyses) already give 1 sample rows). {SAMPLES_CSV_ADVICE}",
                header + 1
            )
        );
        // Within the limit: every run's rows.
        let limits = SamplesCsvLimits { bytes: usize::MAX, ..limits };
        let body = samples_csv_body(&store, "x", 3, limits, Instant::now(), zone()).await.unwrap();
        assert_eq!(body.matches("\r\n").count(), 1 + 3 * 2);
    }

    #[tokio::test]
    async fn a_download_past_its_time_is_a_413_saying_so() {
        let store = experiment_of(2);
        let limits = SamplesCsvLimits { runs: 1_000, bytes: usize::MAX, time: Duration::ZERO };
        let e = samples_csv_body(&store, "x", 2, limits, Instant::now(), zone()).await.unwrap_err();
        assert!(matches!(e, ServiceError::TooLarge(_)), "{e:?}");
        assert_eq!(
            e.to_string(),
            format!(
                "Reading the samples of this experiment's 2 runs (analyses) took longer than the 0 \
                 s one download may take. {SAMPLES_CSV_ADVICE}"
            )
        );
    }

    #[test]
    fn a_state_reads_in_plain_words() {
        assert_eq!(state_words("done"), "Done");
        assert_eq!(state_words("preparing_images"), "Preparing images");
        assert_eq!(state_words(""), "");
    }

    #[test]
    fn a_size_reads_in_mib_when_it_is_whole_mib() {
        assert_eq!(size_words(5 * 1024 * 1024), "5 MiB");
        assert_eq!(size_words(1000), "1000 bytes");
    }

    /// The same cases as `samplesCsvFilename`'s test in `ui/src/lib/experiments/experiments.test.ts`: the
    /// UI saves under the name this header gives.
    #[test]
    fn the_file_name_follows_the_ui_s_rule() {
        for (name, want) in [
            ("Liver tumor study", "Liver tumor study-samples.csv"),
            ("Café 鼠 · batch 2", "Café 鼠 · batch 2-samples.csv"),
            ("a/b\\c:d*e?f\"g<h>i|j\tk", "a-b-c-d-e-f-g-h-i-j-k-samples.csv"),
            ("Liver: batch 1", "Liver- batch 1-samples.csv"),
            ("  ", "experiment-samples.csv"),
            ("", "experiment-samples.csv"),
            (" \u{feff}x\u{85} ", "x--samples.csv"),
        ] {
            assert_eq!(samples_csv_filename(name), want, "{name:?}");
        }
    }
}
