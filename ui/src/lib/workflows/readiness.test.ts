import { describe, it, expect } from 'vitest';
import {
  canRun,
  archMark,
  imageLabel,
  imageStateLabel,
  isBuilt,
  readinessOf,
  shortDigest,
  stateChip,
} from './readiness';

describe('readinessOf', () => {
  it('passes the server state and its reasons through', () => {
    const r = readinessOf({ state: 'pending', not_ready: ['image `fastqc` is being imported'] });
    expect(r).toEqual({ state: 'pending', reasons: ['image `fastqc` is being imported'] });
    expect(canRun(r)).toBe(false);
    expect(stateChip(r)).toEqual({ label: 'building…', cls: 'building' });
  });
  it('drops reasons once ready', () => {
    const r = readinessOf({ state: 'ready', not_ready: ['stale'] });
    expect(r.reasons).toEqual([]);
    expect(canRun(r)).toBe(true);
    expect(stateChip(r)).toBeNull();
  });
  it('shows a failed chip', () => {
    expect(stateChip(readinessOf({ state: 'failed', not_ready: ['x'] }))?.cls).toBe('failed');
  });
  it('treats a server without states as runnable', () => {
    const r = readinessOf({});
    expect(r.state).toBe('unknown');
    expect(canRun(r)).toBe(true);
    expect(stateChip(r)).toBeNull();
    expect(readinessOf({ state: null, not_ready: null })).toEqual({
      state: 'unknown',
      reasons: [],
    });
  });
  it('does not treat a state it does not know as runnable', () => {
    // An open enum: a newer server may add e.g. `archived`, which is a state, not the absence of one.
    const r = readinessOf({ state: 'archived', not_ready: ['version was archived'] });
    expect(r).toEqual({ state: 'other', label: 'archived', reasons: ['version was archived'] });
    expect(canRun(r)).toBe(false);
    expect(stateChip(r)).toEqual({ label: 'archived', cls: 'other' });
  });
});

describe('labels', () => {
  it('names import states', () => {
    expect(imageStateLabel('ready')).toBe('imported');
    expect(imageStateLabel('building')).toBe('importing…');
    expect(imageStateLabel('unbuilt')).toBe('queued');
    expect(imageStateLabel('odd')).toBe('odd');
  });
  it("names a Snakemake image's runtime being added once it is imported", () => {
    const imported = { image: 'repo@sha256:i' };
    expect(imageLabel({ state: 'building', ...imported, derived_state: 'building' })).toBe(
      'adding runtime…',
    );
    expect(imageLabel({ state: 'ready', ...imported, derived_state: 'ready' })).toBe('ready');
    expect(imageLabel({ state: 'failed', ...imported, derived_state: 'failed' })).toBe('failed');
    // Not derived (Nextflow, `runtime: own`), or not imported yet: the import's state.
    expect(imageLabel({ state: 'ready', ...imported })).toBe('imported');
    expect(imageLabel({ state: 'building' })).toBe('importing…');
  });
  it('shortens digests from refs and bare digests', () => {
    const d = 'sha256:' + 'e'.repeat(64);
    expect(shortDigest(`quay.io/x/y:1@${d}`)).toBe('sha256:eeeeeeeeeeee');
    expect(shortDigest(d)).toBe('sha256:eeeeeeeeeeee');
    expect(shortDigest(null)).toBe('--');
  });
});

describe('built images', () => {
  it('say built rather than imported', () => {
    expect(imageLabel({ state: 'building', kind: 'pixi' })).toBe('building…');
    expect(imageLabel({ state: 'ready', kind: 'dockerfile', image: 'r@sha256:x' })).toBe('built');
    expect(imageLabel({ state: 'ready', kind: 'import', image: 'r@sha256:x' })).toBe('imported');
    expect(isBuilt({ kind: 'pixi' })).toBe(true);
    expect(isBuilt({ kind: 'import' })).toBe(false);
    expect(isBuilt({})).toBe(false);
  });
  it('mark each architecture build', () => {
    expect(['succeeded', 'failed', 'running', 'requested', 'unbuilt'].map(archMark)).toEqual([
      '✓',
      '✗',
      '…',
      '…',
      '·',
    ]);
  });
});
