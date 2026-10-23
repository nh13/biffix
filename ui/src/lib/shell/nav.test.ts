import { describe, expect, it } from 'vitest';
import { isCurrent, navFor } from './nav';

describe('navFor', () => {
  it('gives the Lab view the bench’s words and no operator pages', () => {
    const lab = navFor('lab');
    expect(lab.map((i) => i.label)).toEqual([
      'My work',
      'Analyses',
      'Pipelines',
      'Experiments',
      'Data',
      'Settings',
    ]);
    expect(lab.some((i) => i.health)).toBe(false);
  });

  it('gives the Expert view every page, with Status tinted by health', () => {
    const expert = navFor('expert');
    expect(expert.map((i) => i.href)).toEqual([
      '/',
      '/runs',
      '/workflows',
      '/experiments',
      '/data',
      '/usage',
      '/status',
      '/api-docs',
      '/settings',
    ]);
    expect(expert.filter((i) => i.health).map((i) => i.href)).toEqual(['/status']);
  });
});

describe('isCurrent', () => {
  it('marks Home current only on / itself', () => {
    expect(isCurrent('/', '/')).toBe(true);
    expect(isCurrent('/', '/runs')).toBe(false);
  });

  it('marks an entry current on its path and below it, not on a longer sibling', () => {
    expect(isCurrent('/runs', '/runs')).toBe(true);
    expect(isCurrent('/runs', '/runs/r1/jobs')).toBe(true);
    expect(isCurrent('/runs', '/runsx')).toBe(false);
    expect(isCurrent('/settings', '/settings/references')).toBe(true);
  });
});
