import { describe, expect, it } from 'vitest';
import { openOnCreate } from './openOnCreate';

const details = () => ({ open: false }) as HTMLDetailsElement;

describe('openOnCreate', () => {
  it('opens the element on creation when asked', () => {
    const shown = details();
    openOnCreate(shown, true);
    expect(shown.open).toBe(true);
    const closed = details();
    openOnCreate(closed, false);
    expect(closed.open).toBe(false);
  });

  it('never reopens it: a later argument is not applied', () => {
    const node = details();
    const handle = openOnCreate(node, true);
    node.open = false; // the reader closed it
    // Svelte calls `update` with a new argument; there is none to call.
    handle?.update?.(true);
    expect(handle?.update).toBeUndefined();
    expect(node.open).toBe(false);
  });
});
