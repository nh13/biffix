import { projectBucket } from '../lib/buckets.mjs';

// data_sources.rs `DataSource::view`: the project bucket says which of its paths the Data page
// hides (biffix's own prefixes) and which of those it shows after all (the runs' own folders).
// The one copy of the rule here; `hides` (handlers/data.mjs) reads it off the source, as the UI does.
const PROJECT_HIDE_RULE = {
  hidden_prefixes: ['biffix/', 'workflow-sources/'],
  browsable_exceptions: ['biffix/runs/'],
};
/** A project's own bucket as a data source (writable when browsers may upload to it). */
const projectSource = (project, writable) => ({
  id: 'project',
  name: 'Project data',
  description: "This project's own bucket.",
  kind: 'project',
  uri: `s3://${projectBucket(project)}/`,
  writable,
  ...PROJECT_HIDE_RULE,
});
export const DATA_SOURCES = [
  projectSource('genomics', true),
  {
    id: 'core',
    name: 'Sequencing core (deliveries)',
    kind: 'data',
    uri: 's3://seq-core-deliveries/lab-x/',
    writable: false,
  },
  {
    id: 'refs',
    name: 'Reference genomes',
    kind: 'references',
    uri: 's3://shared-refs/',
    writable: false,
  },
  { id: 'locked', name: 'Locked archive', kind: 'data', uri: 's3://denied/', writable: false },
];
// The deploy's projects (`GET /projects`, listed by id as the backend's BTreeMap lists them): the
// default one, whose sources are DATA_SOURCES, and a second with its own bucket and one data
// source, where browsers may not upload.
export const PROJECTS = [
  {
    id: 'genomics',
    bucket: projectBucket('genomics'),
    uploads: true,
    viewer_cors: true,
    default: true,
  },
  {
    id: 'imported',
    bucket: projectBucket('imported'),
    uploads: false,
    viewer_cors: false,
    default: false,
  },
];
const IMPORTED_SOURCES = [
  projectSource('imported', false),
  {
    id: 'bench-data',
    name: 'Benchmark reads',
    kind: 'data',
    uri: 's3://acme-imported-data/',
    writable: false,
  },
];
/** Each project's data sources, by project id. */
export const PROJECT_SOURCES = { genomics: DATA_SOURCES, imported: IMPORTED_SOURCES };
/** The deploy's default project (a run or version that names none is in it). */
export const DEFAULT_PROJECT = PROJECTS.find((p) => p.default).id;
export const DATA_OBJECTS = [
  ...['P01', 'P02', 'P03'].flatMap((s, i) =>
    ['R1', 'R2'].map((r) => ({
      uri: `s3://seq-core-deliveries/lab-x/2026/batch-12/${s}_S${i + 1}_L001_${r}_001.fastq.gz`,
      size: 2_400_000_000 + i * 1000,
    })),
  ),
  { uri: 's3://seq-core-deliveries/lab-x/2026/batch-12/SampleSheet.csv', size: 1843 },
  {
    uri: 's3://seq-core-deliveries/lab-x/2026/batch-13/P04_S1_L001_R1_001.fastq.gz',
    size: 1_900_000_000,
  },
  // A naming slip: one sample's reads as P7 (read 1) and P07 (read 2).
  {
    uri: 's3://seq-core-deliveries/lab-x/2026/batch-14/P7_S7_L001_R1_001.fastq.gz',
    size: 2_000_000_000,
  },
  {
    uri: 's3://seq-core-deliveries/lab-x/2026/batch-14/P07_S7_L001_R2_001.fastq.gz',
    size: 2_000_000_000,
  },
  { uri: 's3://shared-refs/GRCh38/genome.fa', size: 3_200_000_000 },
  // A file with no viewer (download only).
  { uri: 's3://shared-refs/GRCh38/genome.2bit', size: 800_000_000 },
  // Files the Data page can show in place.
  { uri: 's3://shared-refs/GRCh38/stats.tsv', size: 96 },
  { uri: 's3://shared-refs/GRCh38/karyotype.svg', size: 4_000 },
  { uri: 's3://shared-refs/GRCh38/genes.gtf', size: 1_500_000_000 },
  { uri: 's3://genomics-project/uploads/sheet.csv', size: 512 },
  // A file an upload may collide with (uploads never replace an object: 409, rename offered).
  { uri: 's3://genomics-project/uploads/shared/genome.fa', size: 3_000_000_000 },
  // The imported project's bucket and data source (a second project's own listings).
  { uri: 's3://acme-imported-data/hg002/HG002_R1.fastq.gz', size: 5_100_000_000 },
  { uri: 's3://acme-imported-data/hg002/HG002_R2.fastq.gz', size: 5_200_000_000 },
  { uri: 's3://imported-project/results/summary.tsv', size: 96 },
  // A folder with more entries than one listing page (LIST_PAGE), so its listing pages.
  ...Array.from({ length: 1050 }, (_, i) => ({
    uri: `s3://shared-refs/panels/panel-${String(i + 1).padStart(4, '0')}.bed`,
    size: 2_000 + i,
  })),
];
