import fc from 'fast-check';
import { describe, expect, it } from 'vitest';
import { viewParams } from '#lib/util/urlParam.js';
import {
  DEFAULT_OUTPUTS_VIEW,
  outputsViewHref,
  outputsViewParams,
  parseOutputsView,
} from './outputsUrl';

describe('outputs view URL state', () => {
  it('parses nothing as the defaults and serializes the defaults to nothing', () => {
    expect(parseOutputsView(new URLSearchParams())).toEqual(DEFAULT_OUTPUTS_VIEW);
    expect(outputsViewParams(DEFAULT_OUTPUTS_VIEW).toString()).toBe('');
  });

  it('only writes work=1 when work files are shown, and keeps unrelated params', () => {
    expect(outputsViewParams({ ...DEFAULT_OUTPUTS_VIEW, work: true }).toString()).toBe('work=1');
    const params = outputsViewParams(DEFAULT_OUTPUTS_VIEW, new URLSearchParams('x=1&file=a'));
    expect(params.toString()).toBe('x=1');
  });

  it('links a view with no identifier outside the fragment', () => {
    const path = '/runs/r/outputs';
    const view = {
      view: 'flat' as const,
      q: 'NA12878',
      work: true,
      task: 'align:sample=NA12878',
      file: 'out1',
    };
    const href = outputsViewHref(path, view, new URLSearchParams('x=1'));
    const url = new URL(href, 'https://ui.example.test');
    expect(url.pathname).toBe(path);
    expect(url.search).toBe('?x=1&view=flat&work=1');
    expect(url.search).not.toContain('NA12878');
    expect(url.search).not.toContain('out1');
    expect(parseOutputsView(viewParams(url))).toEqual(view);
    expect(outputsViewHref(path, DEFAULT_OUTPUTS_VIEW)).toBe(path);
  });

  it('round-trips every view', () => {
    const text = fc.string({ minLength: 1 }).filter((s) => s.trim() === s);
    fc.assert(
      fc.property(
        fc.record({
          view: fc.constantFrom('tree' as const, 'flat' as const),
          q: fc.oneof(fc.constant(''), text),
          work: fc.boolean(),
          task: fc.option(text, { nil: null }),
          file: fc.option(text, { nil: null }),
        }),
        (view) => {
          expect(parseOutputsView(outputsViewParams(view))).toEqual(view);
          const url = new URL(outputsViewHref('/o', view), 'https://ui.example.test');
          expect(parseOutputsView(viewParams(url))).toEqual(view);
        },
      ),
    );
  });
});
