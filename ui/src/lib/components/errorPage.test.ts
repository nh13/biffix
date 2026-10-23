import { describe, expect, it } from 'vitest';
import { ApiError, AuthRequiredError } from '$lib/api';
import { errorKind, errorWords } from './errorPage';

describe('errorKind', () => {
  it('reads the status of an API error, a status, or else a network failure', () => {
    expect(errorKind(new ApiError('unknown run x', 404))).toBe('not-found');
    expect(errorKind(new ApiError('bad id', 400))).toBe('bad-request');
    expect(errorKind(new ApiError('', 403))).toBe('forbidden');
    expect(errorKind(401)).toBe('signed-out');
    // Only a failed fetch is no connection: an answer that could not be read is the server's side.
    expect(errorKind(new SyntaxError('Unexpected token <'))).toBe('server');
    expect(errorKind(new Error('boom'))).toBe('server');
    expect(errorKind(new ApiError('boom', 500))).toBe('server');
    expect(errorKind(new ApiError('later', 503))).toBe('server');
    expect(errorKind(404)).toBe('not-found');
    expect(errorKind(new TypeError('Failed to fetch'))).toBe('network');
  });
});

describe('errorWords', () => {
  it("speaks the view's words, with a way back", () => {
    const lab = errorWords('not-found', 'run', 'lab');
    expect(lab.title).toBe("This analysis doesn't exist");
    expect(lab.body).toContain('may have been removed');
    expect(lab.back).toEqual({ href: '/runs', label: 'Back to Analyses' });
    expect(lab.retry).toBe(false);
    const expert = errorWords('not-found', 'run', 'expert');
    expect(expert.title).toBe("This run doesn't exist");
    expect(expert.back.label).toBe('Back to Runs');
    expect(errorWords('forbidden', 'workflow', 'lab').title).toBe(
      "You don't have access to this pipeline",
    );
    // A 403 is the role's, not project membership.
    const forbidden = errorWords('forbidden', 'experiment', 'lab').body;
    expect(forbidden).toContain("Your account isn't allowed to see this experiment");
    expect(forbidden).not.toContain('project');
    expect(errorWords('bad-request', 'run', 'lab')).toMatchObject({
      title: "This analysis couldn't be opened",
      retry: false,
    });
    // A sign-in it offers, never one it promises is under way (the loop guard may hold it back).
    expect(errorWords('signed-out', 'run', 'lab')).toMatchObject({
      title: 'Sign in again',
      body: 'Your session has ended. Sign in again to carry on.',
      signIn: true,
    });
    expect(errorWords('server', 'run', 'lab').signIn).toBe(false);
    expect(errorWords('server', 'experiment', 'lab').retry).toBe(true);
    expect(errorWords('network', 'page', 'lab')).toMatchObject({
      title: "biffix can't be reached",
      back: { href: '/', label: 'Back to My work' },
      retry: true,
    });
  });
});
