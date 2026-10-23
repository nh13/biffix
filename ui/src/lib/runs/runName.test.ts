import { describe, expect, it } from 'vitest';
import {
  analysisName,
  derivedName,
  isDerivedName,
  pipelineTitle,
  runName,
  shortDay,
  startTime,
} from './runName';

// Local noon, so the day is the same whatever the test machine's timezone.
const at = (y: number, m: number, d: number) => new Date(y, m - 1, d, 12).getTime() / 1000;
const now = new Date(2026, 9, 4, 12);

describe('pipelineTitle', () => {
  it('prefers the friendly title over the registry name', () => {
    expect(pipelineTitle({ workflow_title: 'Bulk RNA-seq', workflow_name: 'nf-core/rnaseq' })).toBe(
      'Bulk RNA-seq',
    );
    expect(pipelineTitle({ workflow_title: '  ', workflow_name: 'nf-core/rnaseq' })).toBe(
      'nf-core/rnaseq',
    );
    expect(pipelineTitle({})).toBeNull();
    expect(pipelineTitle(null)).toBeNull();
  });
});

describe('shortDay', () => {
  it("names the day, with the year only when it is not this year's", () => {
    expect(shortDay(at(2026, 10, 1), now)).toBe('Oct 1');
    expect(shortDay(at(2025, 12, 31), now)).toBe('Dec 31, 2025');
  });
});

describe('startTime', () => {
  it('is the day and the 24-hour time, with the year only when not this year', () => {
    expect(startTime(new Date(2026, 9, 1, 14, 5).getTime() / 1000, now)).toBe('Oct 1 14:05');
    expect(startTime(new Date(2025, 11, 31, 0, 7).getTime() / 1000, now)).toBe(
      'Dec 31, 2025 00:07',
    );
  });
});

describe('derivedName', () => {
  it('is <pipeline> · <start> · <N samples>, leaving out what is unknown', () => {
    const base = { run_id: 'run_01HGW', workflow_name: 'nf-core/sarek' };
    expect(derivedName({ ...base, submitted_at: at(2026, 10, 1), sampleCount: 2 }, now)).toBe(
      'nf-core/sarek · Oct 1 12:00 · 2 samples',
    );
    expect(
      derivedName(
        {
          ...base,
          workflow_title: 'Variant calling',
          submitted_at: at(2026, 10, 1),
          sampleCount: 1,
        },
        now,
      ),
    ).toBe('Variant calling · Oct 1 12:00 · 1 sample');
    expect(derivedName({ ...base, submitted_at: at(2026, 10, 1), sampleCount: 0 }, now)).toBe(
      'nf-core/sarek · Oct 1 12:00',
    );
    expect(derivedName(base, now)).toBe('nf-core/sarek');
    expect(derivedName({ run_id: 'r', submitted_at: at(2026, 10, 1) }, now)).toBe('Oct 1 12:00');
    expect(derivedName({ run_id: 'r' }, now)).toBeNull();
    // Two unnamed analyses of one pipeline started the same day on the same samples read apart.
    const morning = new Date(2026, 9, 1, 9, 5).getTime() / 1000;
    const afternoon = new Date(2026, 9, 1, 14, 5).getTime() / 1000;
    const a = derivedName({ ...base, submitted_at: morning, sampleCount: 3 }, now);
    const b = derivedName({ ...base, submitted_at: afternoon, sampleCount: 3 }, now);
    expect(a).toBe('nf-core/sarek · Oct 1 09:05 · 3 samples');
    expect(b).toBe('nf-core/sarek · Oct 1 14:05 · 3 samples');
  });
});

describe('analysisName', () => {
  it('is its own name, else the derived one, else its id', () => {
    expect(analysisName({ run_id: 'r', run_name: ' Liver batch 2 ', workflow_name: 'x' })).toBe(
      'Liver batch 2',
    );
    expect(analysisName({ run_id: 'r', run_name: '  ', workflow_name: 'x' }, now)).toBe('x');
    expect(analysisName({ run_id: 'run_01HGW' })).toBe('run_01HGW');
    expect(isDerivedName({ run_id: 'r', run_name: 'n' })).toBe(false);
    expect(isDerivedName({ run_id: 'r', run_name: null })).toBe(true);
  });

  it("reads a run summary's samples", () => {
    expect(
      runName(
        {
          run_id: 'r',
          workflow_name: 'lab-rnaseq',
          workflow_title: 'Bulk RNA-seq',
          submitted_at: at(2026, 9, 30),
          samples: { total: 12 },
        },
        now,
      ),
    ).toBe('Bulk RNA-seq · Sep 30 12:00 · 12 samples');
  });
});
