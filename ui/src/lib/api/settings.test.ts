import { describe, expect, it, vi } from 'vitest';

import { addAlertRecipient, getAlertRecipients, removeAlertRecipient } from './settings';
import { requestedBody, requestedPath, serve, useSignedInSession } from './testing';

// Re-authentication navigates away; the tests only need to know it was started.
vi.mock('./auth', async (importOriginal) => ({
  ...(await importOriginal<typeof import('./auth')>()),
  reauthenticate: vi.fn(async () => true),
}));

useSignedInSession();

describe('alert recipients', () => {
  it('surfaces the server reason on a rejected add and a developer hint on 403', async () => {
    serve(() => new Response('not a valid email address: "x"', { status: 400 }));
    await expect(addAlertRecipient('x')).rejects.toThrow('not a valid email address');
    serve(() => new Response('', { status: 403 }));
    await expect(getAlertRecipients()).rejects.toThrow('Requires developer access');
  });

  it('removes a recipient named in the body, never in the request URL', async () => {
    const fetchFn = serve(() => new Response(null, { status: 204 }));
    await removeAlertRecipient('a+b@example.org');
    expect(requestedPath(fetchFn)).toMatch(/\/settings\/alerts\/remove$/);
    expect(fetchFn.mock.calls[0][1]?.method).toBe('POST');
    expect(requestedBody(fetchFn)).toEqual({ email: 'a+b@example.org' });
    expect(String(fetchFn.mock.calls[0][0])).not.toMatch(/a(%2B|\+)b/);
  });
});
