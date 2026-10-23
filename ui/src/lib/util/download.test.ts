import { afterEach, describe, expect, it, vi } from 'vitest';

import { REVOKE_AFTER_MS, saveBlob } from './download';

describe('saveBlob', () => {
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.useRealTimers();
  });

  it('clicks a link to an object URL named for the file, then revokes the URL later', () => {
    vi.useFakeTimers();
    const link = { href: '', download: '', click: vi.fn() };
    vi.stubGlobal('document', { createElement: vi.fn(() => link) });
    const createObjectURL = vi.fn(() => 'blob:x');
    const revokeObjectURL = vi.fn();
    vi.stubGlobal('URL', { createObjectURL, revokeObjectURL });

    const blob = new Blob(['a,b\n'], { type: 'text/csv' });
    saveBlob(blob, 'usage.csv');

    expect(createObjectURL).toHaveBeenCalledWith(blob);
    expect(link.href).toBe('blob:x');
    expect(link.download).toBe('usage.csv');
    expect(link.click).toHaveBeenCalledOnce();
    // Not in the same task: some browsers drop a save whose URL is revoked at once.
    expect(revokeObjectURL).not.toHaveBeenCalled();
    vi.advanceTimersByTime(REVOKE_AFTER_MS);
    expect(revokeObjectURL).toHaveBeenCalledWith('blob:x');
  });
});
