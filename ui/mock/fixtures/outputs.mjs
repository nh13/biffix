import { outputPrefixOf } from '../lib/buckets.mjs';
import { viewerOf } from '../lib/viewer.mjs';
import { DONE_SAMPLES, NOREF_SAMPLES, OOM_SAMPLES } from './lab.mjs';
import { RUNS } from './runs.mjs';

// Registered outputs per run (POST /runs/:id/outputs/query), shaped like the backend's RunOutput.
// Rows are [path, task_key, role, size, exists = true], under the run's own prefix.
function outputRows(runId, rows, idOf = (i) => `out${i}`) {
  const run = RUNS.find((r) => r.run_id === runId);
  return rows.map(([path, task_key, role, size, exists = true], i) => ({
    id: idOf(i),
    s3_uri: `${outputPrefixOf(run)}${path}`,
    path,
    name: path.split('/').pop(),
    ...(task_key ? { task_key, attempt: 1 } : {}),
    role,
    ...(size != null ? { size } : {}),
    exists,
    servable: true,
    ...(viewerOf(path) ? { viewer: viewerOf(path) } : {}),
  }));
}
export const OUTPUTS = {
  run_01HGW2wgs30x: outputRows('run_01HGW2wgs30x', [
    ['results/align/NA12878.bam', 'align:sample=NA12878', 'output', 38_654_705_664],
    ['results/align/NA12878.bam.bai', 'align:sample=NA12878', 'output', 9_437_184],
    ['results/call/NA12878.vcf.gz', 'call:sample=NA12878', 'output', 187_695_104],
    ['results/call/NA12878.vcf.gz.tbi', 'call:sample=NA12878', 'output', 1_572_864],
    ['logs/align/NA12878.log', 'align:sample=NA12878', 'log', 48_213],
    ['benchmarks/align/NA12878.tsv', 'align:sample=NA12878', 'benchmark', 412],
    ['summary.tsv', null, 'published', null],
    ['qc/NA12878.umi_grouping.metrics', 'align:sample=NA12878', 'output', 1_204],
    ['qc/multiqc_report.html', null, 'published', 5_812_345],
    ['qc/coverage.svg', null, 'published', 2_048],
    ['results/call/NA12878.tmp.bed', 'call:sample=NA12878', 'output', null, false],
    ['work/3f/9a1c/NA12878.sorted.bam', 'align:sample=NA12878', 'intermediate', 38_000_000_000],
  ]),
  // More outputs than one list page (the UI asks for 2,000 at a time), so the list pages.
  run_01HGW9big: outputRows(
    'run_01HGW9big',
    Array.from({ length: 2050 }, (_, i) => [
      `results/shard-${String(i + 1).padStart(4, '0')}.bed`,
      `markdup:sample=S${String(i % 1000).padStart(4, '0')}`,
      'output',
      1_000 + i,
    ]),
    (i) => `big${i}`,
  ),
};
// The Lab analyses' outputs: the finished one's key results (a MultiQC report, a gene-count table,
// a PCA plot, and a FastQC report per sample), and the failed one's per-sample FastQC so far.
// `n(s)` is the number of the sample's task in its run's `tasks` (runs.mjs), so each output names
// the task that wrote it.
const fastqcRows = (samples, n) =>
  samples.map((s) => [`fastqc/${s}_fastqc.html`, `FASTQC (${s}) #${n(s)}`, 'published', 612_345]);
// FastQC's own zip per sample (the data the FastQC viewer reads); for paired-end reads, one per
// read of the pair (`{s}_1_fastqc.zip`, `{s}_2_fastqc.zip`).
const fastqcZipRows = (samples, n, reads = ['']) =>
  samples.flatMap((s) =>
    reads.map((r) => [
      `fastqc/${s}${r}_fastqc.zip`,
      `FASTQC (${s}) #${n(s)}`,
      'published',
      412_345,
    ]),
  );
// A sorted BAM and its index per sample, from each sample's STAR_ALIGN.
const alignmentRows = (samples, dir, n) =>
  samples.flatMap((s) => [
    [`${dir}/${s}.markdup.sorted.bam`, `STAR_ALIGN (${s}) #${n(s)}`, 'output', 3_000_000_000],
    [`${dir}/${s}.markdup.sorted.bam.bai`, `STAR_ALIGN (${s}) #${n(s)}`, 'output', 2_000_000],
  ]);
// The task numbers runs.mjs gives each run's steps: FASTQC, STAR_ALIGN and SALMON_QUANT per sample
// in the finished run; FASTQC then STAR_ALIGN (from #5) in the OOM one; FASTQC, STAR_ALIGN per sample
// in the one without a reference.
const doneTask = (step) => (s) => DONE_SAMPLES.indexOf(s) * 3 + step;
const oomFastqc = (s) => OOM_SAMPLES.indexOf(s) + 1;
const norefTask = (step) => (s) => NOREF_SAMPLES.indexOf(s) * 2 + step;
Object.assign(OUTPUTS, {
  run_01HGWDdone: outputRows('run_01HGWDdone', [
    ['multiqc/multiqc_report.html', 'MULTIQC', 'published', 5_812_345],
    [
      'star_salmon/salmon.merged.gene_counts.tsv',
      'SALMON_QUANT (Tumor_1) #9',
      'published',
      2_210_000,
    ],
    ['star_salmon/pca.svg', null, 'published', 4_096],
    ...fastqcRows(DONE_SAMPLES, doneTask(1)),
    ['star_salmon/Tumor_1.markdup.sorted.bam', 'STAR_ALIGN (Tumor_1) #8', 'output', 3_221_225_472],
    // Appended, so the outputs above keep their ids: each sample's BAM and index (Tumor_1's BAM is
    // above), and FastQC's zips beside the HTML reports — none for Normal_2, whose FastQC opens as
    // its HTML report.
    ['star_salmon/Tumor_1.markdup.sorted.bam.bai', 'STAR_ALIGN (Tumor_1) #8', 'output', 2_097_152],
    ...alignmentRows(['Normal_1', 'Normal_2', 'Tumor_2'], 'star_salmon', doneTask(2)),
    ...fastqcZipRows(['Normal_1', 'Tumor_1', 'Tumor_2'], doneTask(1)),
  ]),
  run_01HGWEoom: outputRows('run_01HGWEoom', fastqcRows(OOM_SAMPLES, oomFastqc)),
  run_01HGWJmqconly: outputRows('run_01HGWJmqconly', [
    ['multiqc/multiqc_report.html', 'MULTIQC', 'published', 2_314_572],
    ['multiqc/multiqc_data/multiqc_fastqc.txt', 'MULTIQC', 'published', 920],
  ]),
  run_01HGWKmqctrim: outputRows('run_01HGWKmqctrim', [
    ['multiqc/multiqc_report.html', 'MULTIQC', 'published', 2_401_113],
    ['multiqc/multiqc_data/multiqc_fastqc_fastqc_raw.txt', 'MULTIQC', 'published', 920],
    ['multiqc/multiqc_data/multiqc_fastqc_fastqc_trimmed.txt', 'MULTIQC', 'published', 916],
    // Not FastQC tables, though their names are close.
    ['multiqc/multiqc_data/multiqc_fastq_screen.txt', 'MULTIQC', 'published', 120],
  ]),
  run_01HGWHnoref: outputRows('run_01HGWHnoref', [
    ...fastqcZipRows(NOREF_SAMPLES, norefTask(1), ['_1', '_2']),
    ...alignmentRows(NOREF_SAMPLES, 'star_salmon', norefTask(2)),
  ]),
});

// A resumed run's outputs, as the backend lists them (outputs.rs `output_scope`): its own rows,
// then those of the run it resumed under the output prefix they share, the run's own winning on a
// URI, each of the earlier run's naming it (`reused_from`). That is a Snakemake resume: Snakemake
// skips a finished task and registers no output for it. (A Nextflow resume differs: nf-biffix
// registers a cache hit's outputs on the resumed run itself, flagged `reused`.)
OUTPUTS.run_01HGW1failed = outputRows(
  'run_01HGW1failed',
  [['qc/BAD01_fastqc.html', 'fastqc:sample=BAD01', 'output', 640_000]],
  (i) => `failed${i}`,
);
OUTPUTS.run_01HGWMsmkresume = [
  ...outputRows(
    'run_01HGWMsmkresume',
    [['aligned/BAD01.bam', 'align:sample=BAD01', 'output', 1_200_000_000]],
    (i) => `smkresume${i}`,
  ),
  ...OUTPUTS.run_01HGW1failed.map((o) => ({ ...o, reused_from: 'run_01HGW1failed' })),
];

// A run's output counts, as the projection keeps them (intermediates, and the files a resumed run
// reused, which it did not register, excluded).
for (const r of RUNS) {
  const rows = (OUTPUTS[r.run_id] ?? []).filter((o) => o.role !== 'intermediate' && !o.reused_from);
  r.outputs_total = rows.length;
  r.outputs_reused = rows.filter((o) => o.reused).length;
}

// Preview text by output path (GET /runs/:id/outputs/:oid/preview).
export const PREVIEWS = {
  'star_salmon/salmon.merged.gene_counts.tsv':
    'gene_id\tgene_name\tNormal_1\tNormal_2\tTumor_1\tTumor_2\nENSMUSG00000029368\tAlb\t48123\t47001\t1204\t980\nENSMUSG00000054932\tAfp\t12\t9\t31902\t27411\nENSMUSG00000034189\tGpc3\t3\t5\t8702\t9133\n',
  'summary.tsv':
    'sample\treads\tmapped_pct\tmean_coverage\nNA12878\t812345678\t99.1\t31.4\nNA12891\t790112233\t98.7\t29.8\nNA12892\t801002003\t99.0\t30.6\n',
  'benchmarks/align/NA12878.tsv':
    's\th:m:s\tmax_rss\tmax_vms\tio_in\tio_out\tmean_load\tcpu_time\n4211.3\t1:10:11\t31402.2\t33980.1\t41200.4\t38102.9\t1523.4\t64120.8\n',
  'qc/NA12878.umi_grouping.metrics':
    '## htsjdk.samtools.metrics.StringHeader\n# fgbio GroupReadsByUmi --input x.bam\n## METRICS CLASS\tcom.fulcrumgenomics.umi.UmiGroupingMetric\nfamily_size\tcount\tfraction\tfraction_gt_or_eq_family_size\n1\t1204\t0.31\t1\n2\t903\t0.23\t0.69\n3\t801\t0.21\t0.46\n10\t12\t0.003\t0.01\n4\t640\t0.16\t0.25\n\n## HISTOGRAM\tjava.lang.Integer\nbin\tvalue\n1\t1204\n2\t903\n',
  'logs/align/NA12878.log':
    '[M::bwa_idx_load_from_disk] read 0 ALT contigs\n[M::process] read 400000 sequences (60000000 bp)...\n[M::mem_process_seqs] Processed 400000 reads in 312.1 CPU sec, 20.4 real sec\n',
};
export const VIEW_URLS = {
  'star_salmon/pca.svg':
    'data:image/svg+xml,' +
    encodeURIComponent(
      '<svg xmlns="http://www.w3.org/2000/svg" width="200" height="120"><rect width="200" height="120" fill="#f6f6f6"/><circle cx="40" cy="40" r="6" fill="#2f5bd0"/><circle cx="55" cy="50" r="6" fill="#2f5bd0"/><circle cx="150" cy="80" r="6" fill="#b42318"/><circle cx="160" cy="70" r="6" fill="#b42318"/></svg>',
    ),
  'multiqc/multiqc_report.html':
    'data:text/html;charset=utf-8,' +
    encodeURIComponent('<h1 style="font-family:sans-serif">MultiQC (mock, lab-rnaseq)</h1>'),
  ...Object.fromEntries(
    ['Normal_1', 'Normal_2', 'Tumor_1', 'Tumor_2', 'Tumor_3', 'Tumor_4'].map((s) => [
      `fastqc/${s}_fastqc.html`,
      'data:text/html;charset=utf-8,' +
        encodeURIComponent(`<h1 style="font-family:sans-serif">FastQC: ${s}</h1>`),
    ]),
  ),
  'qc/multiqc_report.html':
    'data:text/html;charset=utf-8,' +
    encodeURIComponent(
      '<h1 style="font-family:sans-serif">MultiQC (mock)</h1><p id="p">script says: </p><script>document.getElementById("p").textContent += "hello from a sandboxed frame"</script>',
    ),
  'qc/coverage.svg':
    'data:image/svg+xml,' +
    encodeURIComponent(
      '<svg xmlns="http://www.w3.org/2000/svg" width="400" height="160"><rect width="400" height="160" fill="#eef"/><polyline fill="none" stroke="#4a7cf5" stroke-width="3" points="0,140 50,90 100,70 150,60 200,55 250,62 300,80 350,110 400,150"/></svg>',
    ),
};
// Outputs whose files sit in a bucket without a CORS rule for the UI (an imported bucket the deploy
// could not set up): their links say `?mockCors=off`.
export const NO_CORS_OUTPUTS = new Set(['star_salmon/Normal_2.markdup.sorted.bam']);
