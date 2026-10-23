import { describe, expect, it } from 'vitest';
import { expertEquivalent, isExpertOnly, labEquivalent, toggleTarget } from './viewRoutes';

describe('isExpertOnly', () => {
  it('flags a run’s technical tabs and the operator pages', () => {
    for (const p of [
      '/runs/r1/jobs',
      '/runs/r1/outputs',
      '/runs/r1/graph',
      '/runs/r1/coordinator',
      '/runs/r1/cost',
      '/runs/r1/params',
      '/runs/r1/reports',
      '/usage',
      '/status',
      '/api-docs',
      '/settings/references',
      '/workflows/w1/parameters',
      '/workflows/w1/versions',
      '/workflows/w1/provenance',
    ]) {
      expect(isExpertOnly(p), p).toBe(true);
    }
  });

  it('leaves the pages both views share alone', () => {
    for (const p of [
      '/',
      '/runs',
      '/runs/new',
      '/runs/r1',
      '/runs/r1/files',
      '/workflows',
      '/workflows/w1/run',
      '/workflows/w1/about',
      '/workflows/preview',
      '/data/bench/x',
      '/settings',
    ]) {
      expect(isExpertOnly(p), p).toBe(false);
    }
  });
});

describe('labEquivalent / toggleTarget', () => {
  it('sends a run tab to the run’s page and other Expert pages home', () => {
    expect(labEquivalent('/runs/run-9/jobs')).toBe('/runs/run-9');
    expect(labEquivalent('/status')).toBe('/');
    expect(labEquivalent('/workflows/w1/versions')).toBe('/workflows/w1/run');
    // The Outputs tab's Lab page is the analysis's file list.
    expect(labEquivalent('/runs/run-9/outputs')).toBe('/runs/run-9/files');
    expect(labEquivalent('/runs/run-9/files')).toBe('/runs/run-9/files');
  });

  it('keeps the URL when switching to Expert', () => {
    expect(toggleTarget('/runs/run-9', 'expert')).toBe('/runs/run-9');
    expect(toggleTarget('/runs/run-9/jobs', 'lab')).toBe('/runs/run-9');
    expect(toggleTarget('/workflows', 'lab')).toBe('/workflows');
  });

  it('swaps a run’s file list and its Outputs tab', () => {
    expect(toggleTarget('/runs/run-9/files', 'expert')).toBe('/runs/run-9/outputs');
    expect(expertEquivalent('/runs/run-9/files')).toBe('/runs/run-9/outputs');
    expect(expertEquivalent('/runs/new/files')).toBe('/runs/new/files');
    // Only the map's own pages: an inherited key such as `constructor` is no page.
    expect(expertEquivalent('/runs/run-9/constructor')).toBe('/runs/run-9/constructor');
    expect(toggleTarget('/runs/run-9/outputs', 'lab')).toBe('/runs/run-9/files');
  });
});
