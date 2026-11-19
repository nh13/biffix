import { describe, it, expect } from 'vitest';
import { queueRows } from './queues';

describe('queueRows', () => {
  it('lists the default, then labels, then names, each sorted', () => {
    expect(
      queueRows({
        default: 'small-spot',
        by_label: { process_low: 'small-spot', process_high: 'large-spot' },
        by_name: { CALL: 'large-spot', ALIGN: 'large-spot' },
      }),
    ).toEqual([
      { tasks: 'Default', queueClass: 'small-spot' },
      { tasks: 'Label process_high', queueClass: 'large-spot' },
      { tasks: 'Label process_low', queueClass: 'small-spot' },
      { tasks: 'Name ALIGN', queueClass: 'large-spot' },
      { tasks: 'Name CALL', queueClass: 'large-spot' },
    ]);
  });

  it('is empty without an assignment, and leaves out what it does not set', () => {
    expect(queueRows(undefined, ['cpu'])).toEqual([]);
    expect(queueRows(null)).toEqual([]);
    expect(queueRows({ by_name: { align: 'gpu' } })).toEqual([
      { tasks: 'Name align', queueClass: 'gpu' },
    ]);
  });

  it('shows the first queue-class as the default when the assignment names none', () => {
    expect(queueRows({ by_name: { align: 'gpu' } }, ['cpu', 'gpu'])).toEqual([
      { tasks: 'Default (first queue-class)', queueClass: 'cpu' },
      { tasks: 'Name align', queueClass: 'gpu' },
    ]);
    // A default the assignment names wins.
    expect(queueRows({ default: 'gpu' }, ['cpu', 'gpu'])).toEqual([
      { tasks: 'Default', queueClass: 'gpu' },
    ]);
  });
});
