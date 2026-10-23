import { describe, expect, it } from 'vitest';
import { healthOfStatus } from './health';

describe('healthOfStatus', () => {
  it('maps each overall status to its verdict', () => {
    expect(healthOfStatus('ok')).toBe('ok');
    expect(healthOfStatus('degraded')).toBe('warn');
    expect(healthOfStatus('down')).toBe('bad');
  });
  it('reads an unreachable /status as bad', () => {
    expect(healthOfStatus(null)).toBe('bad');
  });
  it('does not call a status it does not know healthy', () => {
    expect(healthOfStatus('maintenance')).toBe('warn');
  });
});
