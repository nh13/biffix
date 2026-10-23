import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { storageWith } from '$lib/api/testing';

// An in-memory Storage for the node test environment (which has no localStorage).
const memStorage = () => storageWith({});

// A Storage that throws on every read and write, as a browser blocking site data does.
const blocked = () => {
  throw new Error('storage blocked');
};
const blockedStorage = { getItem: blocked, setItem: blocked };

// The module reads storage at import time, so each case stubs storage and re-imports it fresh.
describe('prefs.logOrder', () => {
  beforeEach(() => vi.resetModules());
  afterEach(() => vi.unstubAllGlobals());

  it('defaults to oldest-first when nothing is stored', async () => {
    vi.stubGlobal('localStorage', memStorage());
    const { prefs } = await import('./localPrefs.svelte');
    expect(prefs.logOrder).toBe('oldest');
  });

  it('persists a change and restores it on the next load', async () => {
    const store = memStorage();
    vi.stubGlobal('localStorage', store);
    const first = await import('./localPrefs.svelte');
    first.setLogOrder('newest');
    expect(first.prefs.logOrder).toBe('newest');
    expect(store.getItem('biffix-log-order')).toBe('newest');

    vi.resetModules();
    const second = await import('./localPrefs.svelte');
    expect(second.prefs.logOrder).toBe('newest');
  });

  it('falls back to the default, and still applies in-session, when storage throws', async () => {
    vi.stubGlobal('localStorage', blockedStorage);
    const { prefs, setLogOrder } = await import('./localPrefs.svelte');
    expect(prefs.logOrder).toBe('oldest');
    setLogOrder('newest');
    expect(prefs.logOrder).toBe('newest');
  });
});

describe('prefs.showDeveloper', () => {
  beforeEach(() => vi.resetModules());
  afterEach(() => vi.unstubAllGlobals());

  it('is off by default, and a change is remembered on the next load', async () => {
    const store = memStorage();
    vi.stubGlobal('localStorage', store);
    const first = await import('./localPrefs.svelte');
    expect(first.prefs.showDeveloper).toBe(false);
    first.setShowDeveloper(true);
    expect(first.prefs.showDeveloper).toBe(true);
    expect(store.getItem('biffix-show-developer')).toBe('true');
    vi.resetModules();
    const second = await import('./localPrefs.svelte');
    expect(second.prefs.showDeveloper).toBe(true);
  });

  it('still applies in-session when storage throws', async () => {
    vi.stubGlobal('localStorage', blockedStorage);
    const { prefs, setShowDeveloper } = await import('./localPrefs.svelte');
    expect(prefs.showDeveloper).toBe(false);
    setShowDeveloper(true);
    expect(prefs.showDeveloper).toBe(true);
  });
});

describe('theme and sidebar', () => {
  beforeEach(() => vi.resetModules());
  afterEach(() => vi.unstubAllGlobals());

  it('follows the OS until the viewer picks a theme', async () => {
    const store = memStorage();
    vi.stubGlobal('localStorage', store);
    const { savedTheme, saveTheme } = await import('./localPrefs.svelte');
    expect(savedTheme()).toBeNull();
    saveTheme('dark');
    expect(savedTheme()).toBe('dark');
    expect(store.getItem('biffix-theme')).toBe('dark');
  });

  it('ignores a stored theme it does not know', async () => {
    const store = memStorage();
    store.setItem('biffix-theme', 'sepia');
    vi.stubGlobal('localStorage', store);
    const { savedTheme } = await import('./localPrefs.svelte');
    expect(savedTheme()).toBeNull();
  });

  it('remembers the sidebar, and reads blocked storage as nothing saved', async () => {
    vi.stubGlobal('localStorage', memStorage());
    const first = await import('./localPrefs.svelte');
    expect(first.savedSidebarCollapsed()).toBe(false);
    first.saveSidebarCollapsed(true);
    expect(first.savedSidebarCollapsed()).toBe(true);

    vi.stubGlobal('localStorage', blockedStorage);
    expect(first.savedTheme()).toBeNull();
    expect(first.savedSidebarCollapsed()).toBe(false);
    expect(() => first.saveTheme('light')).not.toThrow();
  });
});

describe('the theme choice', () => {
  beforeEach(() => vi.resetModules());
  afterEach(() => vi.unstubAllGlobals());

  it('stores a chosen theme, and forgets it to follow the OS again', async () => {
    const store = memStorage();
    vi.stubGlobal('localStorage', store);
    const { appearance, setThemeChoice } = await import('./localPrefs.svelte');
    setThemeChoice('dark');
    expect(store.getItem('biffix-theme')).toBe('dark');
    // app.html's pre-paint script follows the OS once nothing is stored.
    setThemeChoice('system');
    expect(appearance.theme).toBe('system');
    expect(store.getItem('biffix-theme')).toBeNull();
  });
});
