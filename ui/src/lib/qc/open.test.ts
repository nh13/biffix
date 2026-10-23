import { describe, expect, it } from 'vitest';
import type { RunReport } from '$lib/api';
import type { ViewerFile } from '$lib/components/viewerFile';
import {
  isAlignment,
  isFastqcReport,
  isServable,
  openKind,
  openTarget,
  qcChoices,
  sampleLabels,
  unservedText,
  withoutFastqcTwins,
} from './open';

const report = (name: string, id: string, over: Partial<RunReport> = {}): RunReport => ({
  name,
  kind: 'declared',
  format: 'html',
  output: {
    id,
    name: `${id}.html`,
    path: `${id}.html`,
    s3_uri: `s3://b/${id}.html`,
    role: 'published',
    servable: true,
  },
  ...over,
});

describe('file kinds', () => {
  it('knows alignments and FastQC reports by name', () => {
    expect(isAlignment('a.BAM')).toBe(true);
    expect(isAlignment('a.cram')).toBe(true);
    expect(isAlignment('a.bam.bai')).toBe(false);
    expect(isFastqcReport('S1_fastqc.html')).toBe(true);
    expect(isFastqcReport('S1_fastqc.zip')).toBe(true);
    expect(isFastqcReport('S1.html')).toBe(false);
    // As the server reads it: a sample comes before `_fastqc`.
    expect(isFastqcReport('_fastqc.zip')).toBe(false);
  });
});

describe('opening a result', () => {
  const out = (name: string, viewer?: string) => ({ name, viewer });

  it('opens FastQC in its viewer, a BAM or CRAM in the genome browser, the rest by viewer', () => {
    expect(openKind('fastqc', out('S1_fastqc.html', 'html'))).toBe('fastqc');
    expect(openKind('fastqc', out('S1_fastqc.zip'))).toBe('fastqc');
    expect(openKind('alignment', out('S1.markdup.bam'))).toBe('alignment');
    expect(openKind('alignment', out('S1.CRAM'))).toBe('alignment');
    // An alignment-kind result that is not a BAM/CRAM (its index) opens as any other file would.
    expect(openKind('alignment', out('S1.bam.bai'))).toBeNull();
    expect(openKind('html', out('report.html', 'html'))).toBe('viewer');
    expect(openKind('table', out('counts.tsv', 'table'))).toBe('viewer');
    expect(openKind('file', out('weights.bin'))).toBeNull();
    // A raw FASTQ gets no QC preview of its own.
    expect(openKind('reads', out('S1_R1.fastq.gz'))).toBeNull();
  });

  it('opens the output viewer with its file, the QC viewers with the output and a title', () => {
    const output = report('Per-sample QC', 'fqc').output;
    const file = { name: 'Per-sample QC · S1' } as ViewerFile;
    expect(openTarget('viewer', 'run_1', output, file)).toEqual({ kind: 'viewer', file });
    expect(openTarget('fastqc', 'run_1', output, file)).toEqual({
      kind: 'fastqc',
      runId: 'run_1',
      output,
      title: 'Per-sample QC · S1',
    });
  });
});

describe('qcChoices', () => {
  const bam = (id: string, sample: string | null, name = `${id}.bam`) =>
    report('Alignments', id, {
      format: 'alignment',
      ...(sample ? { sample } : {}),
      output: { ...report('x', id).output, name, viewer: null },
    });

  it('offers each FastQC result and each BAM/CRAM, in order, by sample', () => {
    const { fastqc, alignments } = qcChoices([
      report('QC report', 'multiqc', { kind: 'multiqc' }),
      report('Per-sample QC', 'f1', { format: 'fastqc', sample: 'S1' }),
      report('Per-sample QC', 'f2', { format: 'fastqc', sample: 'S2', kind: 'detected' }),
      bam('b2', 'S2'),
      bam('b1', 'S1'),
      bam('i1', 'S1', 'b1.bam.bai'),
    ]);
    expect(fastqc.map((c) => [c.label, c.report.output.id])).toEqual([
      ['S1', 'f1'],
      ['S2', 'f2'],
    ]);
    expect(alignments.map((c) => [c.label, c.report.output.id])).toEqual([
      ['S2', 'b2'],
      ['S1', 'b1'],
    ]);
  });

  it("names a sample's several files (R1 and R2) by file, and an unsampled one by its name", () => {
    const { fastqc, alignments } = qcChoices([
      report('Per-sample QC', 'S1_R1_fastqc', { format: 'fastqc', sample: 'S1' }),
      report('Per-sample QC', 'S1_R2_fastqc', { format: 'fastqc', sample: 'S1' }),
      bam('merged', null),
    ]);
    expect(fastqc.map((c) => c.label)).toEqual([
      'S1 · S1_R1_fastqc.html',
      'S1 · S1_R2_fastqc.html',
    ]);
    expect(alignments.map((c) => c.label)).toEqual(['merged.bam']);
    expect(qcChoices([])).toEqual({ fastqc: [], alignments: [] });
  });

  it('offers only what can be served: never a file that is gone or not downloadable', () => {
    const gone = bam('gone', 'S1');
    gone.output.exists = false;
    const outside = report('Per-sample QC', 'f9', { format: 'fastqc', sample: 'S9' });
    outside.output.servable = false;
    const { fastqc, alignments } = qcChoices([gone, outside, bam('b2', 'S2')]);
    expect(fastqc).toEqual([]);
    expect(alignments.map((c) => c.report.output.id)).toEqual(['b2']);
  });
});

describe('isServable / unservedText', () => {
  it('serves a stored file biffix issues links for, and says why another is not', () => {
    const o = report('x', 'a').output;
    expect(isServable(o)).toBe(true);
    expect(isServable({ ...o, exists: null })).toBe(true);
    expect(isServable({ ...o, exists: false })).toBe(false);
    expect(isServable({ ...o, servable: false })).toBe(false);
    expect(unservedText({ ...o, exists: false })).toBe('No longer stored');
    expect(unservedText({ ...o, servable: false })).toBe("Can't be downloaded here");
  });
});

describe('sampleLabels', () => {
  it('names entries by sample, a repeated sample by sample and file, else by file', () => {
    const e = (sample: string | null, name: string) => ({ sample, output: { name } });
    expect(
      sampleLabels([
        e('S1', 'S1_1.bam'),
        e('S1', 'S1_2.bam'),
        e('S2', 'S2.bam'),
        e(null, 'all.bam'),
      ]),
    ).toEqual(['S1 · S1_1.bam', 'S1 · S1_2.bam', 'S2', 'all.bam']);
  });
});

describe('withoutFastqcTwins', () => {
  const fq = (path: string, format = 'fastqc') => ({
    format,
    output: { ...report('x', path).output, path },
  });

  it('drops a FastQC zip whose HTML report is listed, whatever the case, and nothing else', () => {
    const kept = withoutFastqcTwins([
      fq('qc/S1_fastqc.html'),
      fq('qc/S1_FASTQC.ZIP'),
      fq('qc/S2_fastqc.zip'),
      fq('other/S1_fastqc.zip'),
      fq('qc/S3_fastqc.zip', 'file'),
      fq('qc/S3_fastqc.html'),
    ]);
    expect(kept.map((r) => r.output.path)).toEqual([
      'qc/S1_fastqc.html',
      'qc/S2_fastqc.zip',
      'other/S1_fastqc.zip',
      'qc/S3_fastqc.zip',
      'qc/S3_fastqc.html',
    ]);
  });

  it('offers one FastQC choice per result in QC at a glance', () => {
    const html = report('Per-sample QC', 'S1_fastqc', { format: 'fastqc', sample: 'S1' });
    const zip = report('Per-sample QC', 'S1_zip', {
      format: 'fastqc',
      sample: 'S1',
      output: { ...html.output, id: 'S1_zip', name: 'S1_fastqc.zip', path: 'S1_fastqc.zip' },
    });
    expect(qcChoices([html, zip]).fastqc.map((c) => [c.label, c.report.output.id])).toEqual([
      ['S1', 'S1_fastqc'],
    ]);
  });
});
