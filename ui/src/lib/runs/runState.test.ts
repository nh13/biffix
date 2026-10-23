import { describe, expect, it } from 'vitest';

import { isLiveRun, isTerminalRun, runStateLabel } from './runState';

describe('run lifecycle', () => {
  it('labels a state in words where its name is not already', () => {
    expect(runStateLabel('preparing_images')).toBe('preparing images');
    expect(runStateLabel('running')).toBe('running');
    expect(runStateLabel('some_future_state')).toBe('some_future_state');
    expect(runStateLabel('constructor')).toBe('constructor');
  });
  it("follows the server's terminal flag", () => {
    expect(isLiveRun({ terminal: false })).toBe(true);
    expect(isTerminalRun({ terminal: false })).toBe(false);
    expect(isLiveRun({ terminal: true })).toBe(false);
    expect(isTerminalRun({ terminal: true })).toBe(true);
  });
  it('treats a run without the fact as not live, so nothing polls forever', () => {
    expect(isLiveRun({})).toBe(false);
    expect(isTerminalRun({})).toBe(true);
  });
});
