import { describe, expect, it } from 'vitest';
import type { RunOutput, RunReport } from '$lib/api';
import {
  fileOpenKind,
  fileTitle,
  filterGroups,
  groupFiles,
  matchesFile,
  shownFiles,
  type LabFile,
} from './files';

const out = (path: string, over: Partial<RunOutput> = {}): RunOutput => ({
  id: path,
  name: path.split('/').pop()!,
  path,
  role: 'output',
  s3_uri: `s3://b/biffix/runs/r/${path}`,
  servable: true,
  ...over,
});
const report = (o: RunOutput, name: string, over: Partial<RunReport> = {}): RunReport => ({
  name,
  kind: 'declared',
  format: 'file',
  output: o,
  ...over,
});

describe('groupFiles', () => {
  const counts = out('star_salmon/salmon.merged.gene_counts.tsv');
  const bam = out('star_salmon/Normal_1.bam');
  const log = out('logs/align.log', { role: 'log' });
  const bench = out('benchmarks/align.tsv', { role: 'benchmark' });
  const work = out('work/ab/x.bam', { role: 'intermediate' });
  const zeta = out('z.txt', { role: 'published' });
  const reports = [report(counts, 'Gene counts', { format: 'table' })];

  it('lists key results first in report order, then other results, then logs, each by path', () => {
    // A report naming a work-directory intermediate is not a key result either.
    const groups = groupFiles(
      [zeta, bench, counts, log, bam, work],
      [...reports, report(work, 'Scratch')],
    );
    expect(groups.map((g) => [g.id, g.title, g.files.map((f) => f.output.path)])).toEqual([
      ['results', 'Key results', ['star_salmon/salmon.merged.gene_counts.tsv']],
      ['other', 'Other results', ['star_salmon/Normal_1.bam', 'z.txt']],
      ['logs', 'Logs and measurements', ['benchmarks/align.tsv', 'logs/align.log']],
    ]);
    expect(groups[0].files[0].result).toEqual({
      name: 'Gene counts',
      sample: null,
      format: 'table',
    });
    expect(groups[1].files[0].folder).toBe('star_salmon/');
  });

  it('lists a key result once, even when two reports name it, and one the list has not loaded', () => {
    const groups = groupFiles([], [...reports, report(counts, 'Counts again')]);
    expect(groups).toHaveLength(1);
    expect(groups[0].files.map((f) => f.result?.name)).toEqual(['Gene counts']);
  });

  it('calls the other files Results when the pipeline declares none, and leaves out empty groups', () => {
    expect(groupFiles([bam], []).map((g) => [g.id, g.title])).toEqual([['other', 'Results']]);
    expect(groupFiles([], [])).toEqual([]);
  });
});

describe('matchesFile / filterGroups', () => {
  const file: LabFile = {
    output: out('star_salmon/Tumor_1.bam'),
    result: { name: 'Alignments', sample: 'Tumor_1', format: 'alignment' },
    folder: 'star_salmon/',
  };
  it('matches the name, folder, result name or sample, ignoring case', () => {
    for (const q of ['tumor_1.BAM', 'STAR_', 'alignm', 'tumor', '  '])
      expect(matchesFile(file, q)).toBe(true);
    expect(matchesFile(file, 'normal')).toBe(false);
  });
  it('drops the groups a search leaves empty', () => {
    const groups = groupFiles([out('a.txt'), out('logs/b.log', { role: 'log' })], []);
    expect(filterGroups(groups, 'b.log').map((g) => g.id)).toEqual(['logs']);
    expect(filterGroups(groups, '')).toEqual(groups);
  });
});

describe('fileOpenKind', () => {
  const plain = (o: RunOutput): LabFile => ({ output: o, result: null, folder: '' });
  it('opens a key result by its format', () => {
    const o = out('a/x.bam');
    expect(
      fileOpenKind({ ...plain(o), result: { name: 'A', sample: null, format: 'alignment' } }),
    ).toBe('alignment');
    expect(
      fileOpenKind({
        ...plain(out('q.html', { viewer: 'html' })),
        result: { name: 'Q', sample: null, format: 'fastqc' },
      }),
    ).toBe('fastqc');
  });
  it('opens other files by their name or viewer', () => {
    expect(fileOpenKind(plain(out('fastqc/S1_fastqc.zip')))).toBe('fastqc');
    expect(fileOpenKind(plain(out('S1.cram')))).toBe('alignment');
    expect(fileOpenKind(plain(out('t.tsv', { viewer: 'table' })))).toBe('viewer');
    expect(fileOpenKind(plain(out('t.bin')))).toBeNull();
  });
  it('never opens a file biffix does not serve or that is gone', () => {
    expect(fileOpenKind(plain(out('t.tsv', { viewer: 'table', servable: false })))).toBeNull();
    expect(fileOpenKind(plain(out('t.tsv', { viewer: 'table', exists: false })))).toBeNull();
  });
});

describe('fileTitle', () => {
  it('names a key result (and its sample), else the file', () => {
    const o = out('a/x.bam');
    expect(fileTitle({ output: o, result: null, folder: 'a/' })).toBe('x.bam');
    expect(
      fileTitle({
        output: o,
        result: { name: 'Alignments', sample: 'S1', format: 'alignment' },
        folder: 'a/',
      }),
    ).toBe('Alignments · S1');
  });
});

describe('shownFiles', () => {
  it('shows a page at a time and says how many are left', () => {
    const files = Array.from({ length: 120 }, (_, i) => i);
    expect(shownFiles(files, 1).shown).toHaveLength(50);
    expect(shownFiles(files, 1).left).toBe(70);
    expect(shownFiles(files, 3)).toEqual({ shown: files, left: 0 });
    expect(shownFiles(files, 0).shown).toHaveLength(50);
  });
});
