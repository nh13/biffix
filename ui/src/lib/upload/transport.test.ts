import { describe, expect, it, vi } from 'vitest';
import { jsonResponse, serve, useSignedInSession } from '$lib/api/testing';
import { apiTransport } from './transport';
import { KeyTaken, PartUrlExpired, UploadGone } from './uploads.svelte';

vi.mock('$lib/api/auth', async (importOriginal) => ({
  ...(await importOriginal<typeof import('$lib/api/auth')>()),
  reauthenticate: vi.fn(async () => true),
}));

useSignedInSession();

// The HTTP answers the upload engine branches on, as the transport turns them into its errors.

// One part PUT, to whatever `serve` answers.
const putPart = () =>
  apiTransport.putPart('https://s3/p/1', new Blob(['ACGT']), new AbortController().signal);

describe('the upload transport', () => {
  it('reads a 404 from the API as an upload that is gone', async () => {
    serve(() => new Response('unknown upload', { status: 404 }));
    await expect(apiTransport.status('u1', 'uploads/a')).rejects.toBeInstanceOf(UploadGone);
    await expect(apiTransport.complete('u1', 'uploads/a', [[1, 'e']])).rejects.toBeInstanceOf(
      UploadGone,
    );
  });

  it('reads a 409 as a taken key, carrying the server text', async () => {
    serve(() => new Response('an object already exists at uploads/a', { status: 409 }));
    for (const call of [
      apiTransport.start('uploads/a', 4, 'text/plain'),
      apiTransport.complete('u1', 'uploads/a', [[1, 'e']]),
    ]) {
      const error = await call.catch((e: unknown) => e);
      expect(error).toBeInstanceOf(KeyTaken);
      expect((error as Error).message).toBe('an object already exists at uploads/a');
    }
  });

  it("reads a part PUT's 403 as an expired URL, and its 404 (NoSuchUpload) as a gone upload", async () => {
    serve(() => new Response('', { status: 403 }));
    await expect(putPart()).rejects.toBeInstanceOf(PartUrlExpired);
    // What S3 said is kept, for the failure a retry could not fix.
    serve(() => new Response('<Error><Code>AccessDenied</Code></Error>', { status: 403 }));
    await expect(putPart()).rejects.toThrow(/^S3 refused the part \(403 AccessDenied\)$/);
    serve(() => new Response('NoSuchUpload', { status: 404 }));
    await expect(putPart()).rejects.toBeInstanceOf(UploadGone);
  });

  it("returns a part's ETag unquoted, and says so when the bucket's CORS hides it", async () => {
    serve(() => new Response('', { status: 200, headers: { etag: '"abc"' } }));
    await expect(putPart()).resolves.toBe('abc');
    serve(() => new Response('', { status: 200 }));
    await expect(putPart()).rejects.toThrow(/CORS rule does not expose ETag/);
  });

  it('says an upload landed only for an object of its size', async () => {
    const answer = (size: number | null, exists: boolean | null) =>
      serve(() => jsonResponse({ objects: [{ uri: 's3://b/uploads/a', exists, size }] }));
    answer(4, true);
    await expect(apiTransport.landed('s3://b/uploads/a', 4)).resolves.toBe(true);
    answer(5, true);
    await expect(apiTransport.landed('s3://b/uploads/a', 4)).resolves.toBe(false);
    // Outside every data source: biffix cannot tell.
    answer(null, null);
    await expect(apiTransport.landed('s3://b/uploads/a', 4)).resolves.toBe(false);
  });
});
