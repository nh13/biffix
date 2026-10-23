import { describe, expect, it } from 'vitest';
import { flagParam, FLAG_ON, validParam, viewParams, withViewParams } from './urlParam';

const TABS = ['parameters', 'versions', 'provenance'] as const;

describe('validParam', () => {
  it('returns the raw value when it is in the allowed set', () => {
    expect(validParam('versions', TABS, 'parameters')).toBe('versions');
  });

  it('falls back for an unrecognized value', () => {
    expect(validParam('bogus', TABS, 'parameters')).toBe('parameters');
  });

  it('falls back for a null (absent) param', () => {
    expect(validParam(null, TABS, 'parameters')).toBe('parameters');
  });

  it('supports a null fallback', () => {
    expect(validParam('name', ['name', 'project'] as const, null)).toBe('name');
    expect(validParam('bogus', ['name', 'project'] as const, null)).toBeNull();
  });
});

const at = (href: string) => new URL(href, 'https://biffix.example');

describe('view state in the fragment', () => {
  it('reads the fragment, filling in from the query string', () => {
    const params = viewParams(at('/runs/r/outputs?view=flat&q=old#q=S1&file=a.bam'));
    expect(Object.fromEntries(params)).toEqual({ view: 'flat', q: 'S1', file: 'a.bam' });
  });

  it('builds links with the view in the fragment, and none when there is no view', () => {
    expect(withViewParams('/runs/r/jobs', new URLSearchParams({ job: 'align:sample=S1' }))).toBe(
      '/runs/r/jobs#job=align%3Asample%3DS1',
    );
    expect(withViewParams('/runs/r/jobs', new URLSearchParams())).toBe('/runs/r/jobs');
  });
});

describe('flagParam', () => {
  it('reads a flag written as the API spells it', () => {
    const on = (q: string) => flagParam(new URLSearchParams(q), 'mine');
    expect(FLAG_ON).toBe('true');
    expect(on('mine=true')).toBe(true);
    expect(on('mine=1')).toBe(false);
    expect(on('mine=false')).toBe(false);
    expect(on('mine=0')).toBe(false);
    expect(on('mine=')).toBe(false);
    expect(on('other=true')).toBe(false);
  });
});
