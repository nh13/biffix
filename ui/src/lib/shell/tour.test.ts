import { describe, expect, it } from 'vitest';
import { TOUR_STOPS, placePopover, shouldShowTour } from './tour';

describe('the tour stops', () => {
  it('are the four of My work, in order', () => {
    // Each with its fallbacks: the welcome for someone with no analyses yet, and the way to the
    // sidebar when its link is not on show (the menu on a narrow screen, Open sidebar when it is
    // collapsed on a wide one).
    expect(TOUR_STOPS.map((s) => s.anchors)).toEqual([
      ['start'],
      ['running', 'welcome'],
      ['nav-pipelines', 'menu', 'reopen'],
      ['view-toggle'],
    ]);
    for (const s of TOUR_STOPS) {
      expect(s.title).not.toBe('');
      expect(s.body).not.toBe('');
    }
  });
});

describe('shouldShowTour', () => {
  it('shows on My work in the Lab view once the preferences say it is not done', () => {
    expect(shouldShowTour('/', 'lab', false)).toBe(true);
  });

  it('waits for the preferences, and never shows once done', () => {
    expect(shouldShowTour('/', 'lab', null)).toBe(false);
    expect(shouldShowTour('/', 'lab', true)).toBe(false);
  });

  it('is for My work in the Lab view only', () => {
    expect(shouldShowTour('/', 'expert', false)).toBe(false);
    expect(shouldShowTour('/runs', 'lab', false)).toBe(false);
    expect(shouldShowTour('/workflows', 'lab', false)).toBe(false);
  });
});

describe('placePopover', () => {
  const size = { width: 300, height: 150 };
  const viewport = { width: 1280, height: 800 };

  it('goes below its anchor, lined up with it', () => {
    const p = placePopover({ top: 100, left: 300, width: 120, height: 40 }, size, viewport);
    expect(p).toEqual({ top: 150, left: 300, centred: false });
  });

  it('lines up with the right edge of an anchor on the right', () => {
    const p = placePopover({ top: 20, left: 1100, width: 120, height: 30 }, size, viewport);
    expect(p.left).toBe(1220 - 300);
    expect(p.centred).toBe(false);
  });

  it('goes above an anchor near the bottom', () => {
    const p = placePopover({ top: 700, left: 100, width: 100, height: 40 }, size, viewport);
    expect(p).toEqual({ top: 700 - 10 - 150, left: 100, centred: false });
  });

  it('is centred when it fits neither below nor above its anchor', () => {
    const centred = { top: 325, left: 490, centred: true };
    // A tall anchor: no room below it or above it.
    const tall = placePopover({ top: 100, left: 100, width: 200, height: 600 }, size, viewport);
    expect(tall).toEqual(centred);
    // An anchor straddling the top edge, too tall to go below.
    const straddling = placePopover(
      { top: -50, left: 100, width: 200, height: 700 },
      size,
      viewport,
    );
    expect(straddling).toEqual(centred);
  });

  it('stays inside a narrow viewport', () => {
    const narrow = { width: 390, height: 800 };
    const p = placePopover({ top: 50, left: 350, width: 30, height: 30 }, size, narrow);
    expect(p.left).toBeGreaterThanOrEqual(8);
    expect(p.left + size.width).toBeLessThanOrEqual(390 - 8);
  });

  it('is centred with no anchor, or one out of view', () => {
    expect(placePopover(null, size, viewport)).toEqual({ top: 325, left: 490, centred: true });
    const off = placePopover({ top: 900, left: 10, width: 50, height: 20 }, size, viewport);
    expect(off.centred).toBe(true);
    const hidden = placePopover({ top: 10, left: 10, width: 0, height: 0 }, size, viewport);
    expect(hidden.centred).toBe(true);
  });
});
