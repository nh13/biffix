import { describe, expect, it } from 'vitest';
import { healthColor, healthWords } from './health';

describe('healthColor', () => {
  it('gives each verdict its color, and none until known', () => {
    expect(healthColor('ok')).toBe('#30a46c');
    expect(healthColor('warn')).toBe('#f5a524');
    expect(healthColor('bad')).toBe('#e5484d');
    expect(healthColor('unknown')).toBe('currentColor');
  });
});

describe('healthWords', () => {
  it('says each verdict, and nothing until known', () => {
    expect(healthWords('ok')).toBe('working');
    expect(healthWords('warn')).toBe('degraded');
    expect(healthWords('bad')).toBe('down or unreachable');
    expect(healthWords('unknown')).toBe('');
  });
});
