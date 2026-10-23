// The deploy's registered workflow versions: those `GET /workflows` lists, their releases and
// typical runs, and versions reached only by id (the Lab analyses' and three still being built).
import { now } from '../lib/clock.mjs';
import { LAB_VERSION_ID } from './lab.mjs';

export const WORKFLOWS = [
  {
    id: 'aabbccddeeff:v1',
    name: 'nf-core/rnaseq',
    project: 'genomics',
    engine: 'nextflow',
    engine_version: '24.10.0',
    git_sha: 'aabbccddeeff',
    image_digest: 'sha256:1111111111111111111111111111111111111111111111111111111111111111',
    image_repository: 'biffix/rnaseq',
    image_registry_id: '123456789012',
    image_region: 'us-east-1',
    queue_classes: ['cpu-spot', 'cpu-ondemand'],
    created_at_micros: (now - 3 * 86400) * 1_000_000,
    created_by: 'alice',
    created_by_label: 'alice@lab.example.org',
    summary: 'RNA-seq analysis: align reads, quantify genes and run QC.',
  },
  {
    id: '112233445566:v2',
    name: 'umi-consensus',
    project: 'imported',
    engine: 'nextflow',
    engine_version: '24.10.0',
    git_sha: '112233445566',
    image_digest: 'sha256:2222222222222222222222222222222222222222222222222222222222222222',
    image_repository: 'biffix/umi-consensus',
    image_registry_id: '123456789012',
    image_region: 'us-east-1',
    queue_classes: ['cpu-spot'],
    created_at_micros: (now - 10 * 86400) * 1_000_000,
    created_by: 'bob',
    created_by_label: 'bob@lab.example.org',
  },
  // A pipeline still in development: no version of it is released, so the Workflows pages leave it
  // out until the viewer turns on Developer.
  {
    id: '7a8b9c0d1e2f:dev1',
    name: 'wgs-germline',
    project: 'genomics',
    engine: 'snakemake',
    engine_version: '9.27.0',
    git_sha: '7a8b9c0d1e2f',
    image_digest: 'sha256:5555555555555555555555555555555555555555555555555555555555555555',
    image_repository: 'biffix/wgs-germline',
    image_registry_id: '123456789012',
    image_region: 'us-east-1',
    queue_classes: ['cpu-spot'],
    created_at_micros: (now - 1 * 86400) * 1_000_000,
    created_by: 'bob',
    created_by_label: 'bob@lab.example.org',
    summary: 'Germline variant calling from whole-genome reads.',
  },
];
// Each version's releases and un-releases (`PUT /workflows/{id}/release`), newest first; a version
// without any is a developer version. Most are: only these two are released.
export const RELEASES = new Map([
  [
    'aabbccddeeff:v1',
    [
      {
        released: true,
        at_micros: (now - 2 * 86400) * 1_000_000,
        by: 'alice',
        by_label: 'alice@lab.example.org',
      },
    ],
  ],
  [
    '112233445566:v2',
    [
      {
        released: true,
        at_micros: (now - 9 * 86400) * 1_000_000,
        by: 'bob',
        by_label: 'bob@lab.example.org',
      },
    ],
  ],
]);

// A typical run of a workflow (`WorkflowEstimate.typical`), by name: from its successful runs.
export const TYPICAL_RUNS = {
  'nf-core/rnaseq': { hours_median: 2.5, usd_median: 14.2, runs: 6 },
  'lab-rnaseq': { hours_median: 3, usd_median: 12.6, runs: 4 },
  // Unpriced history: a time and no cost.
  'nf-core/sarek': { hours_median: 0.5, runs: 3 },
};

// The rnaseq version's About page, with a script and a relative link the UI must neutralize.
export const RNASEQ_ABOUT = `# nf-core/rnaseq

[![CI](https://img.shields.io/badge/ci-passing-green)](https://github.com/nf-core/rnaseq)

RNA-seq analysis: align reads, quantify genes and run QC. See the [nf-core docs](https://nf-co.re/rnaseq).

## Inputs

| Column | What |
|---|---|
| \`sample\` | Sample name |
| \`fastq_1\` | Read 1 FASTQ |

See [usage](docs/usage.md).

<script>window.__aboutScriptRan = true</script>
`;
// The Lab analyses' version (not in the list; reached by id): the rnaseq form, so Try again and
// Inputs used have a real form to read, and its catalog's contact.
export const LAB_VERSION = {
  id: LAB_VERSION_ID,
  name: 'lab-rnaseq',
  project: 'genomics',
  engine: 'nextflow',
  engine_version: '24.10.0',
  git_sha: 'c4d5e6f7a8b9',
  image_digest: 'sha256:4444444444444444444444444444444444444444444444444444444444444444',
  queue_classes: ['cpu-spot'],
  created_at_micros: (now - 20 * 86400) * 1_000_000,
  created_by: 'bob',
  created_by_label: 'bob@lab.example.org',
};

// A version still being built (not in the list; reached by id): its runtime is ready but one of its
// images is importing and another failed, so the Run tab shows why it cannot run.
export const PENDING_VERSION = {
  id: 'ccddeeff0011:pending',
  name: 'fastqc-pending',
  project: 'genomics',
  engine: 'nextflow',
  engine_version: '25.10.4',
  git_sha: 'ccddeeff0011',
  image_digest: 'sha256:' + '3'.repeat(64),
  queue_classes: ['cpu-spot'],
  created_at_micros: (now - 600) * 1_000_000,
};

// A Snakemake version (not in the list; reached by id) whose images are imported: the default
// image is getting the Snakemake runtime added, a named one already has it.
export const DERIVING_VERSION = {
  id: 'ddeeff001122:deriving',
  name: 'samtools-deriving',
  project: 'genomics',
  engine: 'snakemake',
  engine_version: '9.27.0',
  git_sha: 'ddeeff001122',
  image_digest: 'sha256:' + '5'.repeat(64),
  queue_classes: ['cpu-spot'],
  created_at_micros: (now - 300) * 1_000_000,
};

// A Snakemake version (not in the list; reached by id) whose default image is built from a pixi
// environment: amd64 built, arm64 failed. Its named image is built from a Dockerfile.
export const BUILDING_VERSION = {
  id: 'eeff00112233:building',
  name: 'pixi-building',
  project: 'genomics',
  engine: 'snakemake',
  engine_version: '9.27.0',
  git_sha: 'eeff00112233',
  image_digest: '',
  queue_classes: ['cpu-spot'],
  created_at_micros: (now - 200) * 1_000_000,
};
