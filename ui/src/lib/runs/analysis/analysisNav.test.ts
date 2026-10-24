import { describe, expect, it } from 'vitest';
import {
  loadNavList,
  navKey,
  neighbours,
  overlayOpen,
  renameInNavList,
  saveNavList,
  shortcutBlocked,
} from './analysisNav';
import { storageWith } from '#lib/api/testing.js';

const items = [
  { id: 'a', name: 'A' },
  { id: 'b', name: 'B' },
  { id: 'c', name: 'C' },
];

describe('neighbours', () => {
  it('names the analyses either side, none past the ends or for an unlisted one', () => {
    expect(neighbours(items, 'b')).toEqual({ prev: items[0], next: items[2], index: 1 });
    expect(neighbours(items, 'a')).toEqual({ prev: null, next: items[1], index: 0 });
    expect(neighbours(items, 'c')).toEqual({ prev: items[1], next: null, index: 2 });
    expect(neighbours(items, 'z')).toEqual({ prev: null, next: null, index: -1 });
  });
});

describe('saveNavList / loadNavList', () => {
  it('round-trips, and reads a missing or malformed list as none', () => {
    const s = storageWith({});
    expect(loadNavList(s)).toBeNull();
    saveNavList({ items, href: '/runs?filter=failed', more: true }, s);
    expect(loadNavList(s)).toEqual({ items, href: '/runs?filter=failed', more: true });
    // "← Analyses" only ever links back to the list.
    saveNavList({ items, href: 'https://evil.example/' }, s);
    expect(loadNavList(s)).toBeNull();
    saveNavList({ items, href: '/runsx' }, s);
    expect(loadNavList(s)).toBeNull();
    // Nor a page under it, such as a run's own.
    saveNavList({ items, href: '/runs/run_1/files' }, s);
    expect(loadNavList(s)).toBeNull();
    saveNavList({ items, href: '/runs#q=x' }, s);
    expect(loadNavList(s)).toEqual({ items, href: '/runs#q=x', more: false });
    saveNavList({ items, href: '/runs' }, s);
    expect(loadNavList(s)).toEqual({ items, href: '/runs', more: false });
    s.setItem('biffix-analysis-nav', '{"items": 3}');
    expect(loadNavList(s)).toBeNull();
    s.setItem('biffix-analysis-nav', 'not json');
    expect(loadNavList(s)).toBeNull();
    expect(loadNavList(null)).toBeNull();
  });
});

describe('navKey', () => {
  const press = (key: string, tagName = 'BODY', mods = {}) =>
    navKey({ key, target: { tagName } as unknown as EventTarget, ...mods });
  it('reads [ and ] outside fields, without modifiers', () => {
    expect(press('[')).toBe('prev');
    expect(press(']')).toBe('next');
    expect(press('a')).toBeNull();
    expect(press('[', 'INPUT')).toBeNull();
    expect(press(']', 'TEXTAREA')).toBeNull();
    expect(press('[', 'BODY', { metaKey: true })).toBeNull();
  });
  it('leaves a key alone that was handled, repeats, composes, or is pressed behind an overlay', () => {
    expect(press('[', 'BODY', { defaultPrevented: true })).toBeNull();
    expect(press(']', 'BODY', { repeat: true })).toBeNull();
    expect(press(']', 'BODY', { isComposing: true })).toBeNull();
    // Something open over the page (focus may be on the body).
    expect(navKey({ key: ']', target: null }, true)).toBeNull();
    // A target inside a dialog.
    const inDialog = {
      tagName: 'BUTTON',
      closest: (sel: string) => (sel.includes('dialog[open]') ? {} : null),
    } as unknown as EventTarget;
    expect(navKey({ key: ']', target: inDialog })).toBeNull();
    const onPage = { tagName: 'BUTTON', closest: () => null } as unknown as EventTarget;
    expect(navKey({ key: ']', target: onPage })).toBe('next');
  });
});

describe('overlayOpen', () => {
  it('asks the document for an open dialog, modal sheet or drawer', () => {
    const asked: string[] = [];
    const doc = (hit: boolean) => ({
      querySelector: (sel: string) => {
        asked.push(sel);
        return hit ? ({} as Element) : null;
      },
    });
    expect(overlayOpen(doc(true))).toBe(true);
    expect(overlayOpen(doc(false))).toBe(false);
    expect(asked[0]).toContain('dialog[open]');
    expect(asked[0]).toContain('[aria-modal="true"]');
    expect(asked[0]).toContain('.drawer-open');
    expect(overlayOpen(null)).toBe(false);
  });
});

describe('renameInNavList', () => {
  it('renames a remembered row and leaves the rest', () => {
    const s = storageWith({});
    saveNavList({ items, href: '/runs' }, s);
    renameInNavList('b', 'Liver batch', s);
    expect(loadNavList(s)?.items.map((i) => i.name)).toEqual(['A', 'Liver batch', 'C']);
    renameInNavList('z', 'Nope', s);
    expect(loadNavList(s)?.items.map((i) => i.name)).toEqual(['A', 'Liver batch', 'C']);
  });
});

describe('shortcutBlocked', () => {
  it("lets a page's key through on the page, held down or not, and nowhere else", () => {
    const onPage = { tagName: 'BODY', closest: () => null } as unknown as EventTarget;
    expect(shortcutBlocked({ key: 'j', target: onPage })).toBe(false);
    expect(shortcutBlocked({ key: 'j', target: onPage, repeat: true })).toBe(false);
    expect(shortcutBlocked({ key: 'j', target: onPage }, true)).toBe(true);
    expect(shortcutBlocked({ key: 'Escape', target: onPage, defaultPrevented: true })).toBe(true);
    const field = { tagName: 'INPUT' } as unknown as EventTarget;
    expect(shortcutBlocked({ key: 'j', target: field })).toBe(true);
  });
});
