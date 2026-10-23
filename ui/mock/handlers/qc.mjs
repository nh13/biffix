// A run's QC routes (qc.rs): FastQC statuses from its MultiQC report, and MultiQC's general stats.
import { OUTPUTS } from '../fixtures/outputs.mjs';
import { RUNS } from '../fixtures/runs.mjs';
import {
  MULTIQC_FASTQC_TABLES,
  fastqStem,
  fastqcTableSection,
  nonTsvDataFormat,
  owningSample,
  shallowestMultiqc,
} from '../lib/multiqc.mjs';
import { bytewise } from '../lib/paging.mjs';

/**
 * `GET /runs/{run_id}/qc/fastqc-status`: a run's FastQC statuses from MultiQC (every
 * `multiqc_data/multiqc_fastqc*.txt` beside its MultiQC report, the outputs standing in for the
 * backend's S3 listing), as the backend's `qc_fastqc_status`: section by section (the plain table
 * first), each result with the sample that owns its MultiQC name or else its FASTQ's stem, the
 * run's samples first in their order.
 */
function qcFastqcStatus({ params, send }) {
  const runId = params.run_id;
  const run = RUNS.find((r) => r.run_id === runId);
  if (!run) return send(404);
  const rows = OUTPUTS[runId] ?? [];
  const report = shallowestMultiqc(rows);
  if (!report) return send(404, 'this run has no MultiQC report');
  const dir = `${report.path.slice(0, report.path.length - report.name.length)}multiqc_data/`;
  const files = rows
    .filter((o) => o.path.startsWith(dir) && !o.path.slice(dir.length).includes('/'))
    .map((o) => o.name);
  const tables = files
    .map((name) => ({ name, section: fastqcTableSection(name) }))
    .filter((t) => t.section !== undefined)
    .sort(
      (a, b) => Number(a.section != null) - Number(b.section != null) || bytewise(a.name, b.name),
    );
  if (!tables.length) {
    const format = files.map(nonTsvDataFormat).find(Boolean);
    return send(
      404,
      format
        ? `MultiQC wrote its data as ${format} (its \`data_format\`); only TSV is read, so the report's FastQC results cannot be shown`
        : 'the MultiQC report has no FastQC results',
    );
  }
  const ids = [...new Set((run.input_tables?.input?.rows ?? []).map((r) => r.sample))].filter(
    Boolean,
  );
  const rank = (s) => (s.sample == null ? ids.length : ids.indexOf(s.sample));
  const samples = tables.flatMap(({ name, section }) =>
    (MULTIQC_FASTQC_TABLES[name] ?? [])
      .map((s) => {
        const sample =
          owningSample(s.fastqc_name, ids) ??
          (s.filename != null ? owningSample(fastqStem(s.filename), ids) : null);
        return {
          ...(sample == null ? {} : { sample }),
          ...(section == null ? {} : { section }),
          ...s,
        };
      })
      .sort((a, b) => rank(a) - rank(b) || bytewise(a.fastqc_name, b.fastqc_name)),
  );
  send(200, { source: 'multiqc', report_output_id: report.id, samples });
}

/** `GET /runs/{run_id}/qc/general-stats`: a run's MultiQC General Statistics (only the run whose
 * outputs include a MultiQC report). */
function qcGeneralStats({ params, send }) {
  if (!RUNS.some((r) => r.run_id === params.run_id)) return send(404);
  const rows = OUTPUTS[params.run_id] ?? [];
  const report = shallowestMultiqc(rows);
  if (!report) return send(404, 'this run has no MultiQC report');
  send(200, {
    report_output_id: report.id,
    columns: [
      {
        id: 'fastqc-percent_gc',
        title: 'GC',
        namespace: 'FastQC',
        description: 'Average % GC content',
        suffix: '%',
        decimals: 1,
      },
      {
        id: 'fastqc-total_sequences',
        title: 'Seqs',
        namespace: 'FastQC',
        suffix: 'M',
        cond_formatting_rules: { pass: [{ ge: 10 }], fail: [{ lt: 10 }] },
      },
      // Hidden by MultiQC, with a rule NA12878 fails: the Overview does not count a failure the
      // table does not show by default.
      {
        id: 'fastqc-avg_sequence_length',
        title: 'Avg len',
        suffix: ' bp',
        hidden: true,
        cond_formatting_rules: { fail: [{ lt: 200 }] },
      },
    ],
    samples: [
      {
        name: 'NA12878',
        values: {
          'fastqc-percent_gc': 41.2,
          'fastqc-total_sequences': 31.4,
          'fastqc-avg_sequence_length': 150,
        },
      },
      {
        name: 'NA12891',
        values: { 'fastqc-percent_gc': 43.9, 'fastqc-total_sequences': 2.1 },
      },
    ],
  });
}

export const QC_ROUTES = [
  ['GET', '/runs/{run_id}/qc/fastqc-status', qcFastqcStatus],
  ['GET', '/runs/{run_id}/qc/general-stats', qcGeneralStats],
];
