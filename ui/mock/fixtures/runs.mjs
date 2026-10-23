// The deploy's runs: what `GET /runs` lists and `GET /runs/{run_id}` details, a launch adds to and
// a cancel or rename changes.
import { h, now } from '../lib/clock.mjs';
import { DONE_SAMPLES, MQC_SAMPLES, NOREF_SAMPLES, OOM_SAMPLES, labRun, labTask } from './lab.mjs';

// A run's task counters, as the projection keeps them: counted from its tasks (cached is a subset
// of succeeded), so the progress and chips agree with the Jobs view.
export function withTaskCounts(r) {
  const tasks = r.tasks ?? [];
  const n = (st) => tasks.filter((t) => t.state === st).length;
  return Object.assign(r, {
    tasks_observed: tasks.length,
    tasks_succeeded: n('succeeded'),
    tasks_running: n('running'),
    tasks_queued: n('queued'),
    tasks_failed: n('failed'),
    tasks_cached: tasks.filter((t) => t.cached).length,
  });
}

// Submit-time images (`--image`) as the server records them: the spelling as given, and the
// reference it was pinned to.
const UMI_TOOLS = [
  {
    declared_ref: 'ghcr.io/lab/umi-tools:dev',
    source_ref: 'ghcr.io/lab/umi-tools:dev@sha256:um170015',
  },
];
const BWA_MEM3_DEV = [
  {
    declared_ref: 'ghcr.io/lab/bwa-mem3:dev',
    source_ref: 'ghcr.io/lab/bwa-mem3:dev@sha256:bwam3dev',
  },
];
const FEATURE_X_IMAGES = [
  {
    declared_ref: 'ghcr.io/lab/bwa-mem3:feature-x',
    source_ref: 'ghcr.io/lab/bwa-mem3:feature-x@sha256:f3a7ure',
  },
  {
    declared_ref: 'quay.io/biocontainers/samtools:1.22--h50ea8bc_0',
    source_ref: 'quay.io/biocontainers/samtools:1.22--h50ea8bc_0@sha256:5am7001',
  },
];

// Runs that failed before they launched: the `stage` their control-plane `run_failed` recorded
// ("image_preparation" or "launch") and its reason. The failure summary words them as the server does.
export const FAILED_BEFORE_LAUNCH = new Map([
  [
    'run_01HGWAimgfail',
    {
      stage: 'image_preparation',
      reason:
        'image `ghcr.io/lab/bwa-mem3:broken` could not be imported: manifest unknown (build failed)',
    },
  ],
]);

// Each entry is a RunDetail's stored facts (RunSummary fields + tasks/params, and the declared DAG
// as `dag_nodes`/`dag_edges`). The task counters, output counts and the DAG's per-node counts are
// derived (`withTaskCounts`, the output-count loop in fixtures/outputs.mjs, `buildDag`), so they
// always agree with the run's tasks and outputs. GET /runs trims a run to a RunSummary via
// `summary()` (handlers/runs.mjs).
export const RUNS = [
  {
    // A Snakemake run (of a version only runs mention, so its detail is the minimal one).
    run_id: 'run_01HGW3align',
    state: 'running',
    engine: 'snakemake',
    project: 'genomics',
    workflow: 'aabbccddeeff:smk1',
    workflow_name: 'rnaseq-smk',
    submitter: 'user:alice',
    submitter_label: 'alice@lab.example.org',
    submitted_at: now - 2 * h,
    completed_at: null,
    // The engine's own count against its plan (Snakemake knows the whole DAG up front).
    completed: 2,
    total: 12,
    coordinator_job_id: 'coord-abc123',
    coordinator_host: {
      instance_type: 'm6i.large',
      availability_zone: 'us-east-1b',
      capacity: 'on_demand',
      capacity_source: 'instance',
      queue: 'biffix-coordinator',
    },
    input_uris: [
      's3://seq-core-deliveries/lab-x/2026/batch-12/',
      's3://shared-refs/GRCh38/genome.fa',
    ],
    output_prefix: 's3://genomics-project/biffix/runs/run_01HGW3align/',
    provenance: {
      git_sha: 'aabbccddeeff00112233445566778899aabbccdd',
      image_digest: '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:d3r1ved',
      image_source_digest: 'sha256:1mp0rted',
      runtime: {
        engine_version: '9.27.0',
        runtime_key: '4dbb7d79f6bc616bb72c66bafc235e09f91a56ec676e2aba8cdf503c1b93bb56',
        coordinator_image: '123456789012.dkr.ecr.us-east-1.amazonaws.com/coordinator@sha256:c00rd',
      },
      image_map: {
        $default: '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:d3r1ved',
        'quay.io/biocontainers/seqkit:2.9.0--h9ee0642_0':
          '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:s3qk1t',
        'quay.io/biocontainers/samtools:1.21--h50ea8bc_0':
          '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:d3r1ved',
      },
      // Given at submit (`--image`), as pinned: provenance's copy is the anchor's.
      submit_images: UMI_TOOLS,
    },
    // Given at submit (`--image`): from the anchor, or, before launch, from when it was accepted.
    submit_images: UMI_TOOLS,
    cost: {
      estimated_usd: 3.8421,
      compute_usd: 3.51,
      ebs_usd: 0.3321,
      coordinator_usd: 0.19,
      winning_usd: 3.2,
      priced_jobs: 9,
      pending_jobs: 3,
      price_sources: ['on_demand', 'spot_history'],
      by_instance_type: [
        { instance_type: 'c6a.4xlarge', lifecycle: 'spot', jobs: 8, hours: 11.4, usd: 3.6521 },
        { instance_type: 'm6i.large', lifecycle: 'on_demand', jobs: 1, hours: 2.0, usd: 0.19 },
      ],
    },
    params: { genome: 'GRCh38', samples: 6 },
    dag_nodes: ['fastqc', 'align', 'markdup'],
    dag_edges: [
      ['fastqc', 'align'],
      ['align', 'markdup'],
    ],
    tasks: [
      {
        task_key: 'fastqc:sample=HG00096',
        state: 'succeeded',
        attempts: 1,
        group: 'fastqc',
        cost_usd: 0.0041,
        // What its Batch job asked for (`requested_*`); absent ⇒ unknown.
        requested_vcpus: 1,
        requested_memory_mib: 2048,
        started_at: now - 2 * h + 60,
        finished_at: now - 2 * h + 360,
      },
      {
        task_key: 'align:sample=HG00096',
        state: 'running',
        attempts: 2,
        group: 'align',
        started_at: now - 1.5 * h,
        // The display attempt's Batch job and log stream (the attempts endpoint numbers them job-1, job-2).
        batch_job_id: 'job-2',
        log_stream: 'mock/align:sample=HG00096',
        requested_vcpus: 8,
        requested_memory_mib: 30000,
        // The host it runs on; the pane shows its capacity beside the request.
        host: {
          instance_type: 'c6a.4xlarge',
          availability_zone: 'us-east-1b',
          capacity: 'spot',
          registered_vcpus: 16,
          registered_memory_mib: 30720,
        },
        deps: ['fastqc:sample=HG00096'],
      },
      {
        task_key: 'align:sample=HG00097',
        state: 'succeeded',
        attempts: 2,
        group: 'align',
        // The image its engine reported at submit (`task_submitted.image`).
        image: '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:d3r1ved',
        cost_usd: 0.4312,
        requested_vcpus: 8,
        requested_memory_mib: 30000,
        started_at: now - 1.8 * h,
        finished_at: now - 1.2 * h,
      },
      {
        task_key: 'markdup:sample=HG00096',
        state: 'running',
        attempts: 1,
        group: 'markdup',
        started_at: now - 1200,
        deps: ['align:sample=HG00096'],
      },
      // A merge over many samples: its "needs" list is long enough to collapse.
      {
        task_key: 'joint_call',
        state: 'queued',
        // Submitted and RUNNABLE, waiting on capacity: it has a Batch job (job-1) but no host yet,
        // and the job's request is already known.
        attempts: 1,
        group: 'joint_call',
        requested_vcpus: 16,
        requested_memory_mib: 61440,
        deps: [
          'markdup:sample=HG00096',
          'markdup:sample=HG00097',
          'markdup:sample=HG00098',
          'markdup:sample=HG00099',
          'markdup:sample=HG00100',
        ],
      },
    ],
  },
  {
    run_id: 'run_01HGW2wgs30x',
    experiment_id: 'exp_umi_tuning',
    state: 'succeeded',
    engine: 'nextflow',
    project: 'imported',
    workflow: '112233445566:v2',
    workflow_name: 'umi-consensus',
    submitter: 'user:bob',
    submitter_label: 'bob@lab.example.org',
    submitted_at: now - 30 * h,
    completed_at: now - 27 * h,
    completed: 3,
    total: 3,
    coordinator_job_id: 'coord-def456',
    output_prefix: 's3://imported-project/biffix/runs/run_01HGW2wgs30x/',
    params: { reference: 'hs38DH' },
    dag_nodes: ['align', 'call'],
    dag_edges: [['align', 'call']],
    tasks: [
      { task_key: 'align:sample=NA12878', state: 'succeeded', attempts: 1, group: 'align' },
      { task_key: 'call:sample=NA12878', state: 'succeeded', attempts: 1, group: 'call' },
      // A Nextflow process name: upper case, so it sorts first bytewise (a locale order puts it last).
      { task_key: 'MULTIQC', state: 'succeeded', attempts: 1, group: 'MULTIQC' },
    ],
  },
  {
    run_id: 'run_01HGW1failed',
    state: 'failed',
    engine: 'snakemake',
    project: 'imported',
    workflow: '778899aabbcc:v1',
    workflow_name: 'wgs-align',
    submitter: 'watcher:nightly-rnaseq',
    submitter_label: null,
    submitted_at: now - 50 * h,
    completed_at: now - 49 * h,
    completed: 1,
    total: 10,
    coordinator_job_id: 'coord-ghi789',
    // Submitted with `--image`: a resume or re-run submits the same image again, at its digest.
    provenance: {
      git_sha: '778899aabbcc00112233445566778899aabbccdd',
      image_digest: '',
      submit_images: BWA_MEM3_DEV,
    },
    submit_images: BWA_MEM3_DEV,
    params: {},
    dag_nodes: ['fastqc', 'align'],
    dag_edges: [['fastqc', 'align']],
    tasks: [
      {
        task_key: 'align:sample=BAD01',
        state: 'failed',
        attempts: 3,
        group: 'align',
        exit_code: 137,
        termination_category: 'oom',
      },
      { task_key: 'fastqc:sample=BAD01', state: 'succeeded', attempts: 1, group: 'fastqc' },
    ],
  },
  {
    run_id: 'run_01HGW0cancel',
    state: 'cancelled',
    // Who asked to cancel it (a detail field; summaries leave it out).
    cancelled_by: 'alice@lab.example.org',
    engine: 'nextflow',
    project: 'genomics',
    workflow: 'ddeeff001122:v3',
    workflow_name: 'nf-core/sarek',
    submitter: 'user:alice',
    submitter_label: 'alice@lab.example.org',
    submitted_at: now - 72 * h,
    completed_at: now - 71 * h,
    completed: 1,
    total: 8,
    params: {},
    dag_nodes: ['SAY', 'REVERSE'],
    dag_edges: [['SAY', 'REVERSE']],
    tasks: [{ task_key: 'SAY (1)', state: 'succeeded', attempts: 1, group: 'SAY' }],
  },
  {
    // A --resume-from run: it shares the resumed run's prefix and Nextflow session; one task cached.
    run_id: 'run_01HGW4resume',
    state: 'succeeded',
    engine: 'nextflow',
    project: 'genomics',
    workflow: 'ddeeff001122:v3',
    workflow_name: 'nf-core/sarek',
    submitter: 'user:alice',
    submitter_label: 'alice@lab.example.org',
    submitted_at: now - 70 * h,
    completed_at: now - 69 * h,
    completed: 2,
    total: 2,
    output_prefix: 's3://genomics-project/biffix/runs/run_01HGW0cancel/',
    engine_session_id: '0108f2c4-f01e-46f0-b29c-28f12803e2db',
    resumed_from: 'run_01HGW0cancel',
    params: {},
    dag_nodes: ['SAY', 'REVERSE'],
    dag_edges: [['SAY', 'REVERSE']],
    tasks: [
      { task_key: 'SAY (1)', state: 'succeeded', attempts: 0, cached: true, group: 'SAY' },
      {
        task_key: 'REVERSE (1)',
        state: 'succeeded',
        attempts: 1,
        group: 'REVERSE',
        deps: ['SAY (1)'],
      },
    ],
  },
];
// Another run like `base`: of the same version, but none of base's own per-run facts (its output
// prefix, Nextflow session, resume source, host and cost), which `over` gives where it has them.
const runLike = (id, over) => ({
  ...RUNS.find((r) => r.run_id === id),
  output_prefix: undefined,
  engine_session_id: undefined,
  resumed_from: undefined,
  coordinator_host: undefined,
  cost: undefined,
  ...over,
});

// A delete-on-success run whose intermediate files the retention sweep has deleted: it can't resume.
RUNS.push(
  runLike('run_01HGW4resume', {
    run_id: 'run_01HGW6workgone',
    engine_session_id: '6b7c1d2e-3f40-4a51-9b62-7c83d94ea5f6',
    retention: 'delete_on_success',
    work_deleted_at_micros: (now - 60 * h) * 1e6,
  }),
);
// A running run for the mock contract test's cancel (src/mock-contract.test.ts): cancelling
// mutates the mock's state, so no e2e test that expects a live run may use it. The e2e cancel test
// launches a run of its own instead.
RUNS.push(
  runLike('run_01HGW3align', {
    run_id: 'run_01HGW5cancelme',
    submitted_at: now - h,
    completed: 1,
    coordinator_job_id: 'coord-cancelme',
    tasks: RUNS[0].tasks.slice(0, 2),
  }),
);

// A running Nextflow run: no plan total yet, so its progress counts the tasks known so far. Its
// engine reported a session other than the one biffix launched it with.
RUNS.push(
  runLike('run_01HGW4resume', {
    run_id: 'run_01HGW8nflive',
    state: 'running',
    liveClock: true,
    submitted_at: now - 600,
    completed_at: null,
    completed: null,
    total: null,
    coordinator_job_id: 'coord-nflive',
    engine_session_id: '9e1f2a3b-4c5d-4e6f-8a7b-9c0d1e2f3a4b',
    session_mismatch: true,
    tasks: [
      ...['1', '2'].map((n) => ({
        task_key: `SAY (${n})`,
        state: 'succeeded',
        attempts: 1,
        group: 'SAY',
        started_at: now - 540,
        finished_at: now - 480,
      })),
      ...['1', '2', '3'].map((n) => ({
        task_key: `REVERSE (${n})`,
        state: 'running',
        attempts: 1,
        group: 'REVERSE',
        started_at: now - 400 + Number(n) * 30,
      })),
    ],
  }),
);

// A Snakemake resume of the failed run: it re-ran the failed alignment in the failed run's output
// prefix, and skipped the FastQC step that had finished there.
RUNS.push(
  runLike('run_01HGW1failed', {
    run_id: 'run_01HGWMsmkresume',
    state: 'succeeded',
    submitter: 'user:alice',
    submitter_label: 'alice@lab.example.org',
    submitted_at: now - 48 * h,
    completed_at: now - 47 * h,
    completed: 1,
    total: 1,
    coordinator_job_id: 'coord-smkresume',
    output_prefix: 's3://imported-project/biffix/runs/run_01HGW1failed/',
    resumed_from: 'run_01HGW1failed',
    tasks: [{ task_key: 'align:sample=BAD01', state: 'succeeded', attempts: 1, group: 'align' }],
  }),
);

// A run waiting for its submit-time images: not launched yet (no coordinator), one image still
// importing and one getting the Snakemake runtime added.
RUNS.push({
  ...runLike('run_01HGW1failed', {}),
  run_id: 'run_01HGW7preparing',
  state: 'preparing_images',
  submitter: 'user:alice',
  submitter_label: 'alice@lab.example.org',
  submitted_at: now - 900,
  completed_at: null,
  completed: null,
  total: null,
  coordinator_job_id: null,
  dag_nodes: [],
  dag_edges: [],
  tasks: [],
  // Not launched: no provenance (no anchor), only the images it was submitted with.
  provenance: undefined,
  submit_images: FEATURE_X_IMAGES,
  preparing: {
    since_micros: (now - 900) * 1e6,
    not_ready: [
      'image `ghcr.io/lab/bwa-mem3:feature-x` is being imported',
      'image `quay.io/biocontainers/samtools:1.22--h50ea8bc_0`: adding the Snakemake 9.27.0 runtime',
    ],
    images: [
      {
        name: 'ghcr.io/lab/bwa-mem3:feature-x',
        declared_ref: 'ghcr.io/lab/bwa-mem3:feature-x',
        source_ref: 'ghcr.io/lab/bwa-mem3:feature-x@sha256:f3a7ure',
        kind: 'import',
        state: 'building',
        logs_url: 'https://console.aws.amazon.com/codesuite/codebuild/projects/biffix-image-import',
      },
      {
        name: 'quay.io/biocontainers/samtools:1.22--h50ea8bc_0',
        declared_ref: 'quay.io/biocontainers/samtools:1.22--h50ea8bc_0',
        source_ref: 'quay.io/biocontainers/samtools:1.22--h50ea8bc_0@sha256:5am7001',
        kind: 'import',
        state: 'building',
        image: '123456789012.dkr.ecr.us-east-1.amazonaws.com/workflows@sha256:5am7001',
        derived_state: 'building',
      },
    ],
  },
});

// A run that failed while preparing its submit-time images: never launched (no coordinator, no
// provenance), so the failure summary names the image preparation step.
RUNS.push({
  ...RUNS.find((r) => r.run_id === 'run_01HGW7preparing'),
  run_id: 'run_01HGWAimgfail',
  state: 'failed',
  submitted_at: now - 8 * h,
  completed_at: now - 2 * h,
  preparing: undefined,
  submit_images: [
    {
      declared_ref: 'ghcr.io/lab/bwa-mem3:broken',
      source_ref: 'ghcr.io/lab/bwa-mem3:broken@sha256:br0ken',
    },
  ],
});

// A big run (nf-core scale) for paging: 3,000 tasks over three steps, some failed.
{
  const steps = ['fastqc', 'align', 'markdup'];
  const tasks = Array.from({ length: 3000 }, (_, i) => {
    const step = steps[i % 3];
    const sample = `S${String(Math.floor(i / 3)).padStart(4, '0')}`;
    const state = i % 97 === 0 ? 'failed' : i % 13 === 0 ? 'running' : 'succeeded';
    return {
      task_key: `${step}:sample=${sample}`,
      state,
      attempts: state === 'failed' ? 3 : 1,
      group: step,
      ...(step !== 'fastqc'
        ? { deps: [`${steps[steps.indexOf(step) - 1]}:sample=${sample}`] }
        : {}),
    };
  });
  RUNS.push(
    runLike('run_01HGW3align', {
      run_id: 'run_01HGW9big',
      submitted_at: now - 5 * h,
      coordinator_job_id: 'coord-big',
      completed: tasks.filter((t) => t.state === 'succeeded').length,
      total: tasks.length,
      tasks,
    }),
  );
}

// A run where one task's key is a substring of 250 others and sorts after them (off the first
// page), so only an exact-key lookup finds it.
{
  const tasks = [
    ...Array.from({ length: 250 }, (_, i) => ({
      task_key: `align:sample=P${String(i).padStart(3, '0')}`,
      state: 'succeeded',
      attempts: 1,
      group: 'align',
    })),
    { task_key: 'sample=P', state: 'succeeded', attempts: 1, group: 'align' },
  ];
  RUNS.push(
    runLike('run_01HGW3align', {
      run_id: 'run_01HGWBsubstr',
      submitted_at: now - 6 * h,
      completed_at: now - 5 * h,
      state: 'succeeded',
      coordinator_job_id: 'coord-substr',
      completed: tasks.length,
      total: tasks.length,
      tasks,
    }),
  );
}

// The Lab analysis page's analyses (see fixtures/lab.mjs): of an rnaseq version only they reach,
// started by a colleague, older than every other run.
RUNS.push(
  labRun('run_01HGWDdone', DONE_SAMPLES, {
    experiment_id: 'exp_liver_tumor',
    run_name: 'rnaseq · batch-10',
    state: 'succeeded',
    submitted_at: now - 100 * h,
    completed_at: now - 97 * h,
    completed: 13,
    total: 13,
    cost: {
      estimated_usd: 13.8,
      compute_usd: 12.9,
      ebs_usd: 0.9,
      coordinator_usd: 0.3,
      winning_usd: 13.8,
      priced_jobs: 13,
      pending_jobs: 0,
      price_sources: ['on_demand', 'spot_history'],
      by_instance_type: [
        { instance_type: 'c6a.4xlarge', lifecycle: 'spot', jobs: 12, hours: 41.5, usd: 13.5 },
        { instance_type: 'm6i.large', lifecycle: 'on_demand', jobs: 1, hours: 3.0, usd: 0.3 },
      ],
    },
    tasks: [
      ...DONE_SAMPLES.flatMap((s, i) =>
        ['FASTQC', 'STAR_ALIGN', 'SALMON_QUANT'].map((step, j) => labTask(step, s, i * 3 + j + 1)),
      ),
      labTask('MULTIQC', null, 13),
    ],
  }),
  labRun('run_01HGWEoom', OOM_SAMPLES, {
    run_name: 'rnaseq · batch-11',
    state: 'failed',
    submitted_at: now - 99 * h,
    completed_at: now - 96 * h,
    completed: 10,
    total: 13,
    cost: {
      estimated_usd: 11.4,
      compute_usd: 10.8,
      ebs_usd: 0.6,
      coordinator_usd: 0,
      winning_usd: 9.9,
      priced_jobs: 11,
      // Its coordinator is priced on the reconcile tick after the run ended: not final yet.
      pending_jobs: 1,
      price_sources: ['spot_history'],
      by_instance_type: [
        { instance_type: 'c6a.4xlarge', lifecycle: 'spot', jobs: 11, hours: 35.1, usd: 11.4 },
      ],
    },
    tasks: [
      ...OOM_SAMPLES.map((s, i) => labTask('FASTQC', s, i + 1)),
      ...OOM_SAMPLES.map((s, i) =>
        s === 'Tumor_4'
          ? labTask('STAR_ALIGN', s, i + 5, 'failed', {
              attempts: 2,
              exit_code: 137,
              termination_category: 'oom',
            })
          : labTask('STAR_ALIGN', s, i + 5),
      ),
      ...OOM_SAMPLES.slice(0, 3).map((s, i) => labTask('SALMON_QUANT', s, i + 9)),
    ],
  }),
  labRun('run_01HGWFbadin', ['Normal_1', 'Normal_2'], {
    experiment_id: 'exp_liver_tumor',
    run_name: 'rnaseq · batch-12',
    state: 'failed',
    submitted_at: now - 98 * h,
    completed_at: now - 97.5 * h,
    completed: 1,
    total: 7,
    // None of its jobs priced yet (two, its coordinator's among them, are still to be): no cost
    // anywhere yet, and an experiment's total leaves it out, as the backend's does.
    cost: {
      estimated_usd: 0,
      compute_usd: 0,
      ebs_usd: 0,
      coordinator_usd: 0,
      winning_usd: 0,
      priced_jobs: 0,
      pending_jobs: 2,
      price_sources: [],
      by_instance_type: [],
    },
    tasks: [
      labTask('FASTQC', 'Normal_1', 1),
      labTask('FASTQC', 'Normal_2', 2, 'failed', { exit_code: 1 }),
    ],
  }),
);
// A Snakemake analysis whose engine refused a setting before any step ran: no failed step, no
// sample; of a version only this run names, so no pipeline contact.
RUNS.push({
  run_id: 'run_01HGWGengine',
  run_name: 'variants · cohort-3',
  state: 'failed',
  engine: 'snakemake',
  project: 'genomics',
  workflow: 'e4f5a6b7c8d9:v2',
  workflow_name: 'variant-calling',
  submitter: 'user:priya',
  submitter_label: 'priya@lab.example.org',
  submitted_at: now - 97 * h,
  completed_at: now - 96.9 * h,
  completed: 0,
  total: null,
  coordinator_job_id: 'coord-engine',
  params: { min_qual: 'high' },
  dag_nodes: [],
  dag_edges: [],
  tasks: [],
});
// A finished analysis launched without a reference bundle (its alignments ask for one), whose
// pipeline's FastQC results are found by name (not declared).
RUNS.push(
  labRun('run_01HGWHnoref', NOREF_SAMPLES, {
    run_name: 'rnaseq · batch-9',
    state: 'succeeded',
    submitted_at: now - 120 * h,
    completed_at: now - 118 * h,
    completed: 4,
    total: 4,
    reference_bundle: undefined,
    params: {
      input: 's3://genomics-project/biffix/runs/run_01HGWHnoref/inputs/input.csv',
      outdir: 's3://genomics-project/biffix/runs/run_01HGWHnoref/results',
      aligner: 'star_rsem',
    },
    tasks: NOREF_SAMPLES.flatMap((s, i) =>
      ['FASTQC', 'STAR_ALIGN'].map((step, j) => labTask(step, s, i * 2 + j + 1)),
    ),
  }),
);
// A finished analysis whose pipeline published MultiQC's output but not FastQC's own results (no
// `_fastqc.zip`, no `_fastqc.html`): QC at a glance shows each FastQC result's statuses from
// MultiQC's `multiqc_fastqc.txt` (the backend's fixture: S1's pair and S10's R1).
RUNS.push(
  labRun('run_01HGWJmqconly', MQC_SAMPLES, {
    run_name: 'rnaseq · batch-8',
    state: 'succeeded',
    submitted_at: now - 130 * h,
    completed_at: now - 128 * h,
    completed: 3,
    total: 3,
    tasks: [...MQC_SAMPLES.map((s, i) => labTask('FASTQC', s, i + 1)), labTask('MULTIQC', null, 3)],
  }),
);
// The same, with MultiQC's FastQC module run twice, on raw and on trimmed reads (MultiQC's
// documented `module_order` setup, the backend's `multiqc/sections/` fixture): two FastQC
// sections, `fastqc_raw` and `fastqc_trimmed`, holding the same MultiQC names.
RUNS.push(
  labRun('run_01HGWKmqctrim', MQC_SAMPLES, {
    run_name: 'rnaseq · batch-9',
    state: 'succeeded',
    submitted_at: now - 134 * h,
    completed_at: now - 132 * h,
    completed: 3,
    total: 3,
    tasks: [...MQC_SAMPLES.map((s, i) => labTask('FASTQC', s, i + 1)), labTask('MULTIQC', null, 3)],
  }),
);

RUNS.forEach(withTaskCounts);

// Enough runs of one workflow version for several pages of the Runs list, so its keyset Prev/Next
// can be driven. Only a scope to that version (`/runs?workflow=paging-demo:v1`) lists them, so the
// other views' fixtures are unchanged. The runs either side of each 50-row page edge (50/51,
// 100/101) were submitted in the same second, so a page edge falls inside a tie and only the
// run-id half of the keyset (descending) splits it: their ids are swapped so the older of each
// pair still lists second.
export const PAGING_WORKFLOW = 'paging-demo:v1';
// Each ends 30 s after it was submitted (the shifted ones too).
const pagingSubmittedAt = (i) => now - (i + 1 - (i === 50 || i === 100 ? 1 : 0)) * 60;
export const PAGING_RUNS = Array.from({ length: 120 }, (_, i) => ({
  run_id: `run_page_${String(i + 1 + ({ 49: 1, 50: -1, 99: 1, 100: -1 }[i] ?? 0)).padStart(3, '0')}`,
  run_name: `paging run ${i + 1}`,
  state: i % 3 === 0 ? 'failed' : 'succeeded',
  engine: 'snakemake',
  project: 'genomics',
  workflow: PAGING_WORKFLOW,
  workflow_name: 'paging-demo',
  submitter: 'user:alice',
  submitter_label: 'alice@lab.example.org',
  submitted_at: pagingSubmittedAt(i),
  completed_at: pagingSubmittedAt(i) + 30,
  completed: 1,
  total: 1,
  tasks: [
    {
      task_key: 'hello',
      state: i % 3 === 0 ? 'failed' : 'succeeded',
      attempts: 1,
      group: 'hello',
    },
  ],
}));
PAGING_RUNS.forEach(withTaskCounts);
