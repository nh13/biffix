import { describe, expect, it } from 'vitest';
import { asRetention, retentionOptions } from './retention';

describe('retentionOptions', () => {
  it("names the deploy's period as the default", () => {
    expect(retentionOptions(30).map((o) => o.label)).toEqual([
      'Keep for 30 days (default)',
      'Delete when the run succeeds',
      'Keep',
    ]);
    expect(retentionOptions(1)[0].label).toBe('Keep for 1 day (default)');
    expect(retentionOptions(0)[0].label).toBe(
      'Delete when the run ends, even if it fails (default)',
    );
  });

  it('offers Keep once when the deploy keeps them forever', () => {
    expect(retentionOptions(undefined).map((o) => o.value)).toEqual([
      'default',
      'delete_on_success',
    ]);
    expect(retentionOptions(null)[0].label).toBe('Keep (default)');
  });

  it('offers a current Keep the deploy would not, so the choice shows', () => {
    // A re-run of a run that chose Keep, or a draft saved before the period was lifted.
    expect(retentionOptions(null, 'keep').map((o) => o.value)).toEqual([
      'default',
      'delete_on_success',
      'keep',
    ]);
    expect(retentionOptions(null, 'default').map((o) => o.value)).not.toContain('keep');
    expect(retentionOptions(30, 'keep').filter((o) => o.value === 'keep')).toHaveLength(1);
  });

  it('reads a recorded value, defaulting anything unknown', () => {
    expect(asRetention('keep')).toBe('keep');
    expect(asRetention('delete_on_success')).toBe('delete_on_success');
    expect(asRetention(undefined)).toBe('default');
    expect(asRetention('forever')).toBe('default');
  });
});
