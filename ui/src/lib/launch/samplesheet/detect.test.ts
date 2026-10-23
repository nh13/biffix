import { describe, expect, it } from 'vitest';
import type { DetectedSamples } from '#lib/api/index.js';
import {
  autoRule,
  baseName,
  detectionSummary,
  exampleSampleId,
  selectedTokens,
  standingSuggestions,
  toggleToken,
} from './detect';

const response = (over: Partial<DetectedSamples['detection']> = {}): DetectedSamples => ({
  detection: {
    samples: [
      {
        id: 'P01',
        lanes: 1,
        read1: ['s3://b/run/P01_R1.fastq.gz'],
        read2: ['s3://b/run/P01_R2.fastq.gz'],
      },
      {
        id: 'P02',
        lanes: 2,
        read1: ['s3://b/run/P02_R1.fastq.gz'],
        read2: [],
        problems: ['no R2'],
      },
    ],
    ...over,
  },
  example: 'P01_R1.fastq.gz',
  rows: [],
  tokens: [],
});

describe('detected samples', () => {
  it('starts a pick from file names, read automatically', () => {
    expect(autoRule()).toEqual({ id_from: 'name', method: { kind: 'auto' } });
    // A fresh rule each time, so editing one never changes the next pick's.
    expect(autoRule()).not.toBe(autoRule());
  });

  it('summarizes samples, pairs, problems and the files not matched or left out', () => {
    expect(detectionSummary(response())).toBe('2 samples · 1 paired · 1 with problems');
    expect(
      detectionSummary(
        response({ unmatched: ['a', 'b'], skipped: ['s3://b/run/P01_I1.fastq.gz'] }),
      ),
    ).toBe('2 samples · 1 paired · 1 with problems · 2 files not matched · 1 file left out');
    expect(detectionSummary(response({ samples: [] }))).toBe(
      '0 samples · 0 paired · 0 with problems',
    );
  });

  it('names the sample the example file landed in', () => {
    const first = 's3://b/run/P01_R1.fastq.gz';
    expect(exampleSampleId(response(), first)).toBe('P01');
    expect(exampleSampleId(response(), 's3://b/run/other.fastq.gz')).toBeNull();
    expect(exampleSampleId({ ...response(), example: null }, first)).toBeNull();
    expect(exampleSampleId(response(), undefined)).toBeNull();
  });

  it('finds the example by its whole URI, not a base name another folder shares', () => {
    // By parent folder, two samples hold an R1.fastq.gz; the example is the second's.
    const shared = response({
      samples: [
        { id: 'P01', lanes: 1, read1: ['s3://b/P01/R1.fastq.gz'], read2: [] },
        { id: 'P02', lanes: 1, read1: ['s3://b/P02/R1.fastq.gz'], read2: [] },
      ],
    });
    expect(exampleSampleId({ ...shared, example: 'R1.fastq.gz' }, 's3://b/P02/R1.fastq.gz')).toBe(
      'P02',
    );
  });

  it('toggles the parts of a name chosen as the id, kept in name order', () => {
    expect(selectedTokens(autoRule())).toEqual([]);
    expect(selectedTokens({ method: { kind: 'tokens', selected: [2, 0] } })).toEqual([2, 0]);
    expect(toggleToken([], 2)).toEqual([2]);
    expect(toggleToken([2], 0)).toEqual([0, 2]);
    expect(toggleToken([0, 2], 2)).toEqual([0]);
  });

  it('keeps a merge suggestion only while both of its samples are still rows', () => {
    const s = [
      { ids: ['P7', 'P07'] as [string, string], into: 'P07' },
      { ids: ['A', 'a'] as [string, string], into: 'A' },
    ];
    const rows = [{ sample: 'P7' }, { sample: 'P07' }, { sample: 'A' }];
    expect(standingSuggestions(s, rows, 'sample')).toEqual([s[0]]);
    expect(standingSuggestions(s, rows, null)).toEqual([]);
  });

  it("reads a URI's file name", () => {
    expect(baseName('s3://b/run/P01_R1.fastq.gz')).toBe('P01_R1.fastq.gz');
    expect(baseName('P01_R1.fastq.gz')).toBe('P01_R1.fastq.gz');
  });
});
