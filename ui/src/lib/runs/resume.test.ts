import { describe, expect, it } from 'vitest';

import { resumeCommand, type ResumeFacts } from './resume';

const finished: ResumeFacts = { run_id: 'run-b', workflow: 'abc:def', resumable: true };

describe('resumeCommand', () => {
  it('builds the CLI command for a run the server says is resumable', () => {
    expect(resumeCommand(finished)).toBe('biffix runs submit abc:def --resume-from run-b');
  });
  it('is null when the server says it cannot be resumed, or says nothing', () => {
    expect(resumeCommand({ ...finished, resumable: false })).toBeNull();
    expect(resumeCommand({ run_id: 'run-b', workflow: 'abc:def' })).toBeNull();
  });
  it('is null without a workflow version to submit', () => {
    expect(resumeCommand({ ...finished, workflow: null })).toBeNull();
  });
});
