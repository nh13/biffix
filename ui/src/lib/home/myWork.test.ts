import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { storageWith } from '$lib/api/testing';
import {
  choiceAfter,
  rememberChoice,
  rememberedChoice,
  rowOpen,
  seeAllHref,
  workSummary,
  WORK_ROWS,
} from './myWork';

describe('My work rows', () => {
  it('are Needs attention, In progress and Recent results, each seeing all in its filter', () => {
    expect(WORK_ROWS.map((r) => r.title)).toEqual([
      'Needs attention',
      'In progress',
      'Recent results',
    ]);
    expect(WORK_ROWS.map(seeAllHref)).toEqual([
      '/runs?filter=attention',
      '/runs?filter=active',
      '/runs?filter=succeeded',
    ]);
  });

  it('open Needs attention when it has something, and keep the rest closed', () => {
    expect(rowOpen('attention', null, 2)).toBe(true);
    expect(rowOpen('attention', null, 0)).toBe(false);
    expect(rowOpen('running', null, 3)).toBe(false);
    expect(rowOpen('finished', null, 9)).toBe(false);
    expect(rowOpen('experiments', null, 4)).toBe(false);
  });

  it('never reopens a closed Experiments row on its own, whatever its count', () => {
    // Only Needs attention reopens when more need a look; the same object comes back unchanged.
    const closed = { open: false, seen: 1 };
    expect(choiceAfter('experiments', closed, 5)).toBe(closed);
  });

  it('open as the viewer left them', () => {
    expect(rowOpen('attention', { open: false, seen: 2 }, 2)).toBe(false);
    expect(rowOpen('running', { open: true, seen: 0 }, 0)).toBe(true);
  });
});

describe('choiceAfter', () => {
  const closed = { open: false, seen: 2 };

  it('opens Needs attention again when more need a look than when it was closed', () => {
    expect(choiceAfter('attention', closed, 3)).toBeNull();
    expect(rowOpen('attention', choiceAfter('attention', closed, 3), 3)).toBe(true);
  });

  it('keeps the choice while the count holds, and lowers what was seen as it falls', () => {
    expect(choiceAfter('attention', closed, 2)).toBe(closed);
    const fell = choiceAfter('attention', closed, 1);
    expect(fell).toEqual({ open: false, seen: 1 });
    // A rise from there is news, though not past the count first seen.
    expect(choiceAfter('attention', fell, 2)).toBeNull();
  });

  it('leaves an open row, the other rows and no choice alone', () => {
    const open = { open: true, seen: 2 };
    expect(choiceAfter('attention', open, 5)).toBe(open);
    // A fall still lowers what was seen, and the row stays open.
    expect(choiceAfter('attention', open, 1)).toEqual({ open: true, seen: 1 });
    expect(choiceAfter('running', closed, 9)).toBe(closed);
    expect(choiceAfter('attention', null, 4)).toBeNull();
  });
});

describe('remembering a row', () => {
  // The node test environment has no localStorage: an in-memory one.
  beforeEach(() => vi.stubGlobal('localStorage', storageWith({})));
  afterEach(() => vi.unstubAllGlobals());

  it('stores open or closed and the count per row, and nothing until the viewer chooses', () => {
    expect(rememberedChoice('running')).toBeNull();
    rememberChoice('running', { open: true, seen: 4 });
    rememberChoice('attention', { open: false, seen: 2 });
    expect(rememberedChoice('running')).toEqual({ open: true, seen: 4 });
    expect(rememberedChoice('attention')).toEqual({ open: false, seen: 2 });
    expect(rememberedChoice('finished')).toBeNull();
    rememberChoice('attention', null);
    expect(rememberedChoice('attention')).toBeNull();
  });

  it('reads a choice stored without its count as never news', () => {
    localStorage.setItem('biffix-mywork-attention', 'closed');
    expect(rememberedChoice('attention')).toEqual({ open: false, seen: Infinity });
    expect(choiceAfter('attention', rememberedChoice('attention'), 50)).toEqual({
      open: false,
      seen: 50,
    });
  });

  it('starts every row at its default when storage is refused', () => {
    vi.stubGlobal('localStorage', {
      getItem: () => {
        throw new Error('denied');
      },
      setItem: () => {
        throw new Error('denied');
      },
    });
    expect(() => rememberChoice('running', { open: true, seen: 1 })).not.toThrow();
    expect(rememberedChoice('running')).toBeNull();
  });
});

describe('workSummary', () => {
  it('counts what is running and what needs a look', () => {
    expect(workSummary(2, 1)).toBe('2 running, 1 needs a look.');
    expect(workSummary(0, 3)).toBe('3 need a look.');
    expect(workSummary(1, 0)).toBe('1 running.');
    expect(workSummary(0, 0)).toBe('');
  });
});
