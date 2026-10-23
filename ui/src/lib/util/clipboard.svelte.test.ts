import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { CopiedFlag, copyText } from './clipboard.svelte';

function stubClipboard(writeText: (t: string) => Promise<void>) {
  vi.stubGlobal('navigator', { clipboard: { writeText } });
}

describe('copyText', () => {
  afterEach(() => vi.unstubAllGlobals());

  it('reports success', async () => {
    const written: string[] = [];
    stubClipboard(async (t) => void written.push(t));
    expect(await copyText('abc')).toBe(true);
    expect(written).toEqual(['abc']);
  });

  it('reports a refusal instead of throwing', async () => {
    stubClipboard(() => Promise.reject(new Error('denied')));
    expect(await copyText('abc')).toBe(false);
  });

  it('reports a browser without a clipboard', async () => {
    vi.stubGlobal('navigator', {});
    expect(await copyText('abc')).toBe(false);
  });
});

describe('CopiedFlag', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it('marks the button that copied, then clears', async () => {
    stubClipboard(async () => {});
    const flag = new CopiedFlag();
    await flag.copy('s3://b/k', 'row1');
    expect(flag.is('row1')).toBe(true);
    expect(flag.is('row2')).toBe(false);
    vi.advanceTimersByTime(1500);
    expect(flag.is('row1')).toBe(false);
  });

  it('a later copy moves the mark and restarts the clock', async () => {
    stubClipboard(async () => {});
    const flag = new CopiedFlag();
    await flag.copy('a');
    vi.advanceTimersByTime(1000);
    await flag.copy('b');
    vi.advanceTimersByTime(1000);
    expect(flag.is('b')).toBe(true);
    expect(flag.is('a')).toBe(false);
  });

  it('marks nothing when the copy failed', async () => {
    stubClipboard(() => Promise.reject(new Error('denied')));
    const flag = new CopiedFlag();
    expect(await flag.copy('a')).toBe(false);
    expect(flag.is('a')).toBe(false);
  });
});
