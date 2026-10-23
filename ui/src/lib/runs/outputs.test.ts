import { describe, expect, it } from 'vitest';

import type { RunOutput } from '$lib/api';
import {
  buildOutputTree,
  initiallyOpen,
  matchesSearch,
  reusedFromSummary,
  roleLabel,
  sameOutputs,
  visibleRows,
} from './outputs';

function out(path: string, size?: number, task_key?: string): RunOutput {
  const name = path.split('/').pop() ?? path;
  return {
    id: path,
    s3_uri: `s3://b/p/${path}`,
    path,
    name,
    role: 'output',
    servable: true,
    ...(size != null ? { size } : {}),
    ...(task_key ? { task_key } : {}),
  };
}

describe('buildOutputTree', () => {
  it('nests by path segment and rolls up counts and known sizes', () => {
    const root = buildOutputTree([
      out('results/b/x.tsv', 10),
      out('results/a.bam', 100),
      out('results/b/y.tsv'),
      out('top.txt', 1),
    ]);
    expect(root.fileCount).toBe(4);
    expect(root.knownBytes).toBe(111);
    expect(root.files.map((f) => f.name)).toEqual(['top.txt']);
    const results = root.dirs[0];
    expect(results.path).toBe('results/');
    expect(results.files.map((f) => f.name)).toEqual(['a.bam']);
    const b = results.dirs[0];
    expect([b.path, b.fileCount, b.knownBytes]).toEqual(['results/b/', 2, 10]);
  });

  it('reads leading, trailing and doubled slashes as nothing, and a slash-only path at the root', () => {
    const root = buildOutputTree([out('/a//b.txt'), out('c/'), out('//')]);
    expect(root.dirs.map((d) => [d.path, d.files.map((f) => f.name)])).toEqual([['a/', ['b.txt']]]);
    expect(root.files.map((f) => f.path).sort()).toEqual(['//', 'c/']);
    expect(root.fileCount).toBe(3);
  });

  it('sorts directories and files by name', () => {
    const root = buildOutputTree([out('z/1'), out('a/1'), out('m.txt'), out('c.txt')]);
    expect(root.dirs.map((d) => d.name)).toEqual(['a', 'z']);
    expect(root.files.map((f) => f.name)).toEqual(['c.txt', 'm.txt']);
  });

  it('keeps one directory per name in a wide directory', () => {
    const outputs = Array.from({ length: 300 }, (_, i) => out(`s${i % 100}/f${i}.txt`, 1));
    const root = buildOutputTree(outputs);
    expect(root.dirs).toHaveLength(100);
    expect(root.dirs.every((d) => d.fileCount === 3)).toBe(true);
    expect(root.knownBytes).toBe(300);
  });
});

describe('roleLabel', () => {
  it('shows intermediates as work and every other role as named', () => {
    expect(roleLabel('intermediate')).toBe('work');
    expect(roleLabel('output')).toBe('output');
    expect(roleLabel('published')).toBe('published');
    expect(roleLabel('someNewRole')).toBe('someNewRole');
  });
});

describe('visibleRows', () => {
  const root = buildOutputTree([out('a/b/c.txt'), out('a/d.txt'), out('e.txt')]);

  it('descends only into open directories', () => {
    const collapsed = visibleRows(root, new Set());
    expect(collapsed.map((r) => (r.kind === 'dir' ? r.dir.path : r.file.path))).toEqual([
      'a/',
      'e.txt',
    ]);
    const opened = visibleRows(root, new Set(['a/']));
    expect(opened.map((r) => [r.kind === 'dir' ? r.dir.path : r.file.path, r.depth])).toEqual([
      ['a/', 0],
      ['a/b/', 1],
      ['a/d.txt', 1],
      ['e.txt', 0],
    ]);
  });

  it('opens the top levels initially', () => {
    expect([...initiallyOpen(root, 1)]).toEqual(['a/']);
    expect([...initiallyOpen(root, 2)].sort()).toEqual(['a/', 'a/b/']);
  });
});

describe('matchesSearch', () => {
  it('matches the path or the task, case-insensitively', () => {
    const o = out('results/Sample1.bam', 1, 'align:sample=S1');
    expect(matchesSearch(o, 'sample1')).toBe(true);
    expect(matchesSearch(o, 'ALIGN')).toBe(true);
    expect(matchesSearch(o, 'vcf')).toBe(false);
    expect(matchesSearch(o, '  ')).toBe(true);
  });
});

describe('sameOutputs', () => {
  it('is true only for the same rows with the same fields', () => {
    const a = [out('a.bam', 10, 'align'), out('b.bam')];
    expect(
      sameOutputs(
        a,
        a.map((o) => ({ ...o })),
      ),
    ).toBe(true);
    expect(sameOutputs(a, [a[0]])).toBe(false);
    expect(sameOutputs(a, [a[1], a[0]])).toBe(false);
    // A size arriving, a file going missing: the same ids, a changed field.
    expect(sameOutputs(a, [a[0], { ...a[1], size: 5 }])).toBe(false);
    expect(sameOutputs(a, [a[0], { ...a[1], exists: false }])).toBe(false);
    expect(sameOutputs(a, [{ ...a[0], reused: true }, a[1]])).toBe(false);
    // Same keys, one value changed: only the per-field comparison can see it.
    expect(sameOutputs(a, [{ ...a[0], size: 11 }, a[1]])).toBe(false);
    expect(sameOutputs(a, [{ ...a[0], task_key: 'sort' }, a[1]])).toBe(false);
  });
});

describe('reusedFromSummary', () => {
  it('counts the outputs earlier runs wrote, naming the run when there is one', () => {
    const own = out('own.txt');
    const from = (path: string, run: string) => ({ ...out(path), reused_from: run });
    expect(reusedFromSummary([own])).toBe('');
    expect(reusedFromSummary([own, from('a.bam', 'run-a'), from('b.bam', 'run-a')])).toBe(
      '2 reused from run-a',
    );
    expect(reusedFromSummary([from('a.bam', 'run-a'), from('b.bam', 'run-b'), own])).toBe(
      '2 reused from 2 earlier runs',
    );
  });
});
