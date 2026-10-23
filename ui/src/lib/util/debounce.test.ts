import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { debounce, delayed } from './debounce';

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe('debounce', () => {
  it('calls once the calls pause, with the last arguments', () => {
    const fn = vi.fn();
    const d = debounce(fn, 250);
    d('a');
    vi.advanceTimersByTime(200);
    d('b');
    vi.advanceTimersByTime(200);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(50);
    expect(fn).toHaveBeenCalledOnce();
    expect(fn).toHaveBeenCalledWith('b');
  });

  it('cancel drops a waiting call, and later calls still go through', () => {
    const fn = vi.fn();
    const d = debounce(fn, 250);
    d();
    d.cancel();
    vi.advanceTimersByTime(1000);
    expect(fn).not.toHaveBeenCalled();
    d();
    vi.advanceTimersByTime(250);
    expect(fn).toHaveBeenCalledOnce();
  });
});

describe('delayed', () => {
  it('calls after the delay unless cancelled first', () => {
    const fn = vi.fn();
    delayed(fn, 300);
    const cancel = delayed(fn, 300);
    cancel();
    vi.advanceTimersByTime(300);
    expect(fn).toHaveBeenCalledOnce();
  });
});
