import { describe, expect, it } from 'vitest';
import { costBasis, imageMapRows, paramEntries, submitImageRows } from './runFacts';

describe('paramEntries', () => {
  it('sorts an object by key', () => {
    expect(paramEntries({ b: 2, a: { x: 1 } })).toEqual([
      ['a', { x: 1 }],
      ['b', 2],
    ]);
  });
  it('is empty for anything that is not a plain object', () => {
    expect(paramEntries(undefined)).toEqual([]);
    expect(paramEntries(null)).toEqual([]);
    expect(paramEntries([1, 2] as never)).toEqual([]);
  });
});

describe('costBasis', () => {
  const cost = {
    priced_jobs: 3,
    pending_jobs: 0,
    price_sources: ['on_demand'],
  } as unknown as Parameters<typeof costBasis>[0];

  it('names the price sources and the exclusions', () => {
    const text = costBasis(cost);
    expect(text).toMatch(/^Estimated from 3 jobs: on-demand list price\. /);
    expect(text).toMatch(/Not the bill\.$/);
    expect(text).not.toMatch(/not priced yet/);
  });
  it('mentions unpriced jobs', () => {
    expect(costBasis({ ...cost!, pending_jobs: 2 })).toMatch(/2 jobs not priced yet\./);
    expect(costBasis({ ...cost!, priced_jobs: 1, pending_jobs: 1 })).toMatch(
      /^Estimated from 1 job: .* 1 job not priced yet\./,
    );
  });
  it('is empty without an estimate', () => {
    expect(costBasis(null)).toBe('');
  });
});

describe('imageMapRows', () => {
  it('groups references by the image they run and drops identity and biffix keys', () => {
    expect(
      imageMapRows({
        'quay.io/b/seqkit:2': 'repo@sha256:s',
        'quay.io/b/seqkit:2 ': 'repo@sha256:s',
        'repo@sha256:s': 'repo@sha256:s',
        $default: 'repo@sha256:d',
        'quay.io/b/samtools:1': 'repo@sha256:d',
      }),
    ).toEqual([
      { image: 'repo@sha256:d', refs: ['quay.io/b/samtools:1'] },
      { image: 'repo@sha256:s', refs: ['quay.io/b/seqkit:2', 'quay.io/b/seqkit:2 '] },
    ]);
  });
  it('is empty without a map', () => {
    expect(imageMapRows(undefined)).toEqual([]);
    expect(imageMapRows(null)).toEqual([]);
  });
});

describe('submitImageRows', () => {
  it('pairs each reference as given with what it was pinned to, sorted by reference', () => {
    expect(
      submitImageRows([
        { declared_ref: 'quay.io/b/seqkit:2', source_ref: 'quay.io/b/seqkit:2@sha256:s' },
        { declared_ref: 'ghcr.io/lab/bwa:dev', source_ref: 'ghcr.io/lab/bwa:dev@sha256:b' },
      ]),
    ).toEqual([
      { declared: 'ghcr.io/lab/bwa:dev', pinned: 'ghcr.io/lab/bwa:dev@sha256:b' },
      { declared: 'quay.io/b/seqkit:2', pinned: 'quay.io/b/seqkit:2@sha256:s' },
    ]);
  });
  it('lists each spelling once', () => {
    const image = {
      declared_ref: 'ubuntu:24.04',
      source_ref: 'docker.io/library/ubuntu:24.04@sha256:u',
    };
    expect(submitImageRows([image, { ...image }])).toEqual([
      { declared: 'ubuntu:24.04', pinned: 'docker.io/library/ubuntu:24.04@sha256:u' },
    ]);
  });
  it('is empty when the submit named none', () => {
    expect(submitImageRows(undefined)).toEqual([]);
    expect(submitImageRows(null)).toEqual([]);
    expect(submitImageRows([])).toEqual([]);
  });
});
