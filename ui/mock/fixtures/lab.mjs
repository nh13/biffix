import { h, now } from '../lib/clock.mjs';

// The Lab analysis page's results and failures: analyses (their runs are pushed in
// fixtures/runs.mjs) of an rnaseq version that only these runs reach (`LAB_VERSION`, not listed, so
// the catalog's counts are unchanged), started by a colleague (so My work is unchanged), older than
// every other run (so they list last). One finished with key results (a per-sample one among them),
// one failed on memory (resumable), one on a damaged input, and one whose engine refused a setting
// (of a version with no contact of its own).
export const LAB_VERSION_ID = 'c4d5e6f7a8b9:v1';
const labSamples = (ids) =>
  ids.map((sample) => ({
    sample,
    fastq_1: `s3://seq-core-deliveries/lab-x/batch-11/${sample}_R1.fastq.gz`,
    fastq_2: `s3://seq-core-deliveries/lab-x/batch-11/${sample}_R2.fastq.gz`,
    strandedness: 'auto',
  }));
export const labTask = (step, sample, n, state = 'succeeded', over = {}) => ({
  task_key: sample ? `${step} (${sample}) #${n}` : step,
  state,
  attempts: 1,
  group: step,
  ...over,
});
const LAB_STEPS = ['FASTQC', 'STAR_ALIGN', 'SALMON_QUANT', 'MULTIQC'];
export const labRun = (runId, ids, over) => ({
  run_id: runId,
  engine: 'nextflow',
  project: 'genomics',
  workflow: LAB_VERSION_ID,
  workflow_name: 'lab-rnaseq',
  submitter: 'user:priya',
  submitter_label: 'priya@lab.example.org',
  coordinator_job_id: `coord-${runId}`,
  engine_session_id: `5e551011-${runId.slice(-4)}-4000-8000-000000000000`,
  input_uris: ['s3://seq-core-deliveries/lab-x/batch-11/'],
  params: {
    input: `s3://genomics-project/biffix/runs/${runId}/inputs/input.csv`,
    outdir: `s3://genomics-project/biffix/runs/${runId}/results`,
    fasta: 's3://shared-refs/GRCm39/genome.fa',
    gtf: 's3://shared-refs/GRCm39/genes.gtf',
    aligner: 'star_rsem',
    min_trimmed_reads: 100000,
  },
  input_tables: {
    input: {
      uri: `s3://genomics-project/biffix/runs/${runId}/inputs/input.csv`,
      rows: labSamples(ids),
    },
  },
  reference_bundle: {
    id: 'grcm39',
    uris: { fasta: 's3://shared-refs/GRCm39/genome.fa', gtf: 's3://shared-refs/GRCm39/genes.gtf' },
  },
  dag_nodes: LAB_STEPS,
  dag_edges: [
    ['FASTQC', 'STAR_ALIGN'],
    ['STAR_ALIGN', 'SALMON_QUANT'],
    ['SALMON_QUANT', 'MULTIQC'],
  ],
  ...over,
});
// The analyses' samples (their runs are in fixtures/runs.mjs): the finished one's, the one that ran
// out of memory, one launched without a reference bundle, and one with MultiQC's results only.
export const DONE_SAMPLES = ['Normal_1', 'Normal_2', 'Tumor_1', 'Tumor_2'];
export const OOM_SAMPLES = ['Tumor_1', 'Tumor_2', 'Tumor_3', 'Tumor_4'];
export const NOREF_SAMPLES = ['Liver_1', 'Liver_2'];
export const MQC_SAMPLES = ['S1', 'S10'];
// The key results they declare (`reports:`), as `RunReport`s are built: `{sample}` expands over
// `samples` (a per-sample result: each sample's files the path matches, `*` any name part, up to
// four in path order — a paired-end sample's two), and one missing from the outputs is left out.
const labReports = (samples) => [
  { name: 'QC report', path: 'multiqc/multiqc_report.html', kind: 'multiqc', format: 'html' },
  {
    name: 'Gene counts',
    path: 'star_salmon/salmon.merged.gene_counts.tsv',
    kind: 'declared',
    format: 'table',
    description: 'Reads per gene, per sample',
  },
  {
    name: 'PCA',
    path: 'star_salmon/pca.svg',
    kind: 'declared',
    format: 'image',
    description: 'Samples by expression',
  },
  {
    name: 'Per-sample QC',
    path: 'fastqc/{sample}_fastqc.html',
    kind: 'declared',
    format: 'fastqc',
    description: 'FastQC',
    samples,
  },
  {
    name: 'Alignments',
    path: 'star_salmon/{sample}.markdup.sorted.bam',
    kind: 'declared',
    format: 'alignment',
    samples,
  },
];
// The declared ones as a form freezes them (`FormReport`): what a version's `reports:` says, the
// MultiQC report among them (a declaration that is the run's MultiQC report is listed as `multiqc`
// under its declared name), so a run report's `declaration` indexes this list.
export const declaredReports = () =>
  labReports([]).map(({ name, path, format, description }) => ({
    name,
    path,
    kind: format,
    ...(description ? { description } : {}),
    per_sample: path.includes('{sample}'),
  }));
export const LAB_REPORTS = {
  run_01HGWDdone: labReports(DONE_SAMPLES),
  // Only its MultiQC report exists of what it declares.
  run_01HGWJmqconly: labReports(MQC_SAMPLES),
  run_01HGWKmqctrim: labReports(MQC_SAMPLES),
  run_01HGWEoom: labReports(OOM_SAMPLES),
  // Its declared FastQC reports (HTML) were not written; the zips are found by name, two per
  // sample (paired-end reads: each sample's `_1` and `_2`).
  run_01HGWHnoref: [
    ...labReports(NOREF_SAMPLES),
    {
      name: 'Per-sample QC',
      path: 'fastqc/{sample}_*_fastqc.zip',
      kind: 'detected',
      format: 'fastqc',
      samples: NOREF_SAMPLES,
    },
  ],
};

// Tries as the attempts endpoint records them beyond what it makes up from a task's count, by
// run id and task key: the OOM analysis's failed alignment was first lost to a spot reclaim after
// 40 min, then its retry ran out of memory after 1 h 10 min.
export const ATTEMPT_VIEWS = new Map([
  [
    'run_01HGWEoom\u0000STAR_ALIGN (Tumor_4) #8',
    [
      {
        started_at: now - 98.5 * h,
        finished_at: now - 98.5 * h + 40 * 60,
        spot_warned_at: now - 98.5 * h + 38 * 60,
        termination_category: 'spot',
        host: { capacity: 'spot', capacity_source: 'instance' },
      },
      {
        started_at: now - 97.7 * h,
        finished_at: now - 97.7 * h + 70 * 60,
        exit_code: 137,
        termination_category: 'oom',
        host: { capacity: 'spot', capacity_source: 'instance' },
      },
    ],
  ],
]);

// How each of these failed, as `failure_summary` words it (keyed by run).
export const LAB_FAILURES = {
  run_01HGWEoom: {
    failed_steps: 1,
    items: [
      {
        task_key: 'STAR_ALIGN (Tumor_4) #8',
        sample: 'Tumor_4',
        step: 'STAR_ALIGN',
        cause: 'out_of_memory',
        message: 'it ran out of memory',
        advice:
          'Give this step more memory (its resources in the workflow), or split its input into smaller pieces.',
        attempts: 2,
        exit_code: 137,
        batch_job_id: '5f0c6a2e-7b1d-4c9e-9a51-2f3e8d4b6c01',
        reason: 'OutOfMemoryError: Container killed due to memory usage',
        log_tail: [
          'STAR --runThreadN 8 --genomeDir /ref/star --readFilesIn Tumor_4_R1.fq.gz Tumor_4_R2.fq.gz',
          '.. started STAR run',
          '.. loading genome',
          '.. started mapping',
          'EXITING because of fatal ERROR: not enough memory for BAM sorting',
          'Killed',
        ],
        try_again: 'resume',
      },
    ],
  },
  run_01HGWFbadin: {
    failed_steps: 1,
    items: [
      {
        task_key: 'FASTQC (Normal_2) #2',
        sample: 'Normal_2',
        step: 'FASTQC',
        cause: 'bad_input',
        message: 'one of its input files looks damaged or is not in the format it expects',
        advice: "Download or upload this sample's files again; the log below names the file.",
        attempts: 1,
        exit_code: 1,
        batch_job_id: '8a7d3c1b-2e4f-4a6b-8c9d-0e1f2a3b4c5d',
        log_tail: ['gzip: Normal_2_R1.fastq.gz: unexpected end of file'],
        try_again: 'fix_samples',
      },
    ],
  },
  run_01HGWGengine: {
    failed_steps: 0,
    items: [
      {
        step: 'workflow engine',
        cause: 'engine_error',
        message: 'the workflow engine stopped with an error before any step failed',
        advice: "The engine's log, below, says why: often a parameter or input it could not use.",
        attempts: 1,
        log_tail: ["ValueError: min_qual: 'high' is not a number"],
        try_again: 'fix_options',
      },
    ],
  },
};
