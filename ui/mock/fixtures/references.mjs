import { now } from '../lib/clock.mjs';

// Reference bundles (Settings → References; the Run tab's reference dropdown). One lacks a GTF, so
// the rnaseq form does not offer it; one is an assay kit.
const bundleAt = (created) => ({
  created_at_micros: (now - created * 86400) * 1_000_000,
  updated_at_micros: (now - created * 86400) * 1_000_000,
  created_by: 'alice',
});
export const REFERENCES = [
  {
    id: 'grch38-ensembl110',
    kind: 'assembly',
    display_name: 'Human GRCh38 · Ensembl 110',
    uris: {
      fasta: 's3://shared-refs/GRCh38/genome.fa',
      gtf: 's3://shared-refs/GRCh38/genes.gtf',
      star_index: 's3://shared-refs/GRCh38/star/',
    },
    notes: 'Primary assembly, no ALTs.',
    ...bundleAt(30),
  },
  {
    id: 'grcm39',
    kind: 'assembly',
    display_name: 'Mouse GRCm39 · Ensembl 110',
    uris: { fasta: 's3://shared-refs/GRCm39/genome.fa', gtf: 's3://shared-refs/GRCm39/genes.gtf' },
    ...bundleAt(20),
  },
  {
    id: 'hg38-gatk',
    kind: 'assembly',
    display_name: 'Human hg38 (GATK, no annotation)',
    uris: { fasta: 's3://shared-refs/hg38/Homo_sapiens_assembly38.fasta' },
    ...bundleAt(10),
  },
  {
    id: 'hg38-igv',
    kind: 'assembly',
    display_name: 'Human hg38 (hosted in the viewer)',
    uris: { fasta: 's3://shared-refs/hg38/Homo_sapiens_assembly38.fasta' },
    igv_genome: 'hg38',
    ...bundleAt(8),
  },
  {
    id: 'twist-exome-v2',
    kind: 'assay_kit',
    display_name: 'Twist Exome v2',
    uris: { targets_bed: 's3://shared-refs/kits/twist-exome-v2/targets.bed' },
    ...bundleAt(5),
  },
];
