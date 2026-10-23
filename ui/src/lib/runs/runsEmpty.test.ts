import { describe, expect, it } from 'vitest';
import { nothingStartedYet, type EmptyListOf } from './runsEmpty';

const mine: EmptyListOf = {
  lab: true,
  showEveryone: false,
  experimentScope: '',
  workflowScope: '',
  filter: 'all',
  search: '',
};

describe('nothingStartedYet', () => {
  it('is true for the Lab view’s own, unfiltered list', () => {
    expect(nothingStartedYet(mine)).toBe(true);
    expect(nothingStartedYet({ ...mine, search: '  ' })).toBe(true);
  });

  it('is false once anything narrows or widens the list, or in the Expert view', () => {
    expect(nothingStartedYet({ ...mine, lab: false })).toBe(false);
    expect(nothingStartedYet({ ...mine, showEveryone: true })).toBe(false);
    expect(nothingStartedYet({ ...mine, experimentScope: 'e1' })).toBe(false);
    expect(nothingStartedYet({ ...mine, workflowScope: 'wf-v2' })).toBe(false);
    expect(nothingStartedYet({ ...mine, filter: 'attention' })).toBe(false);
    expect(nothingStartedYet({ ...mine, search: 'wgs' })).toBe(false);
  });
});
