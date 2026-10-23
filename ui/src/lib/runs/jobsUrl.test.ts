import fc from 'fast-check';
import { describe, expect, it } from 'vitest';
import {
  DEFAULT_JOBS_VIEW,
  JOBS_VIEW_KEYS,
  jobsHref,
  jobsViewHref,
  jobsViewParams,
  parseJobsView,
  STATE_FILTERS,
  TASK_SORTS,
} from './jobsUrl';
import { viewParams } from '#lib/util/urlParam.js';

describe('jobs view URL state', () => {
  it('parses empty params as the defaults and serializes the defaults to nothing', () => {
    expect(parseJobsView(new URLSearchParams())).toEqual(DEFAULT_JOBS_VIEW);
    expect(jobsViewParams(DEFAULT_JOBS_VIEW).toString()).toBe('');
  });

  it('falls back on unknown values', () => {
    const view = parseJobsView(new URLSearchParams('state=done&sort=size&order=up'));
    expect(view.state).toBe('all');
    expect(view.sort).toBe('key');
    expect(view.descending).toBe(false);
  });

  it('keeps unrelated params and drops cleared ones', () => {
    const params = jobsViewParams(
      { ...DEFAULT_JOBS_VIEW, q: '  ' },
      new URLSearchParams('keep=1&group=align&q=x'),
    );
    expect(params.toString()).toBe('keep=1');
  });

  it('round-trips every view', () => {
    const key = fc.string({ minLength: 1 }).filter((s) => s.trim() === s);
    fc.assert(
      fc.property(
        fc.record({
          state: fc.constantFrom(...STATE_FILTERS),
          group: fc.option(key, { nil: null }),
          q: fc.oneof(fc.constant(''), key),
          sort: fc.constantFrom(...TASK_SORTS),
          descending: fc.boolean(),
          job: fc.option(key, { nil: null }),
        }),
        (view) => {
          expect(parseJobsView(jobsViewParams(view))).toEqual(view);
        },
      ),
    );
  });

  it('owns every key it writes', () => {
    const params = jobsViewParams({
      state: 'failed',
      group: 'align',
      q: 'x',
      sort: 'state',
      descending: true,
      job: 'align:sample=S1',
    });
    expect([...params.keys()].sort()).toEqual([...JOBS_VIEW_KEYS].sort());
  });
});

describe('jobsHref', () => {
  it('links to the Jobs view with the view in the fragment, never the query', () => {
    const href = jobsHref('run 1', { job: 'align:sample=NA12878', q: 'NA12878' });
    const url = new URL(href, 'https://biffix.example');
    expect(url.pathname).toBe('/runs/run%201/jobs');
    expect(url.search).toBe('');
    expect(url.pathname + url.search).not.toContain('NA12878');
    expect(parseJobsView(viewParams(url))).toEqual({
      ...DEFAULT_JOBS_VIEW,
      job: 'align:sample=NA12878',
      q: 'NA12878',
    });
  });

  it('is the bare tab for the default view', () => {
    expect(jobsHref('r')).toBe('/runs/r/jobs');
    expect(jobsHref('r', { state: 'failed' })).toBe('/runs/r/jobs#state=failed');
  });
});

describe('jobsViewHref', () => {
  it('writes the view to the fragment and drops view keys a query-form link carried', () => {
    const url = new URL('https://biffix.example/runs/r/jobs?state=failed&keep=1#q=x');
    const href = jobsViewHref(url, DEFAULT_JOBS_VIEW);
    expect(href).toBe('/runs/r/jobs?keep=1');
    // Clearing the filter really clears it: the old query no longer fills it back in.
    expect(parseJobsView(viewParams(new URL(href, url)))).toEqual(DEFAULT_JOBS_VIEW);
    expect(jobsViewHref(url, { ...DEFAULT_JOBS_VIEW, state: 'active', q: 'y' })).toBe(
      '/runs/r/jobs?keep=1#q=y&state=active',
    );
  });
});
