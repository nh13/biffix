// The upload engine's transport: the API's upload endpoints, and the part PUTs straight to S3
// (presigned, so they carry no biffix credentials). Every API call names the object key in its
// POST body, never in the request URL (a file name can name a sample).

import {
  authedFetch,
  headDataObjects,
  projectField,
  responseError,
  uploadPaths,
  type StartedUpload,
  type UploadedFile,
  type UploadPartUrlList,
  type UploadStatus,
} from '#lib/api/index.js';
import type { components } from '#lib/generated/api.js';
import { KeyTaken, PartUrlExpired, UploadGone, type UploadTransport } from './uploads.svelte';

type Schemas = components['schemas'];

/** An upload API call, through the app's one authenticated request path (an expired session
 * re-authenticates and throws `AuthRequiredError`). */
async function post(path: string, body: unknown): Promise<Response> {
  const response = await authedFetch(path, { method: 'POST', body: JSON.stringify(body) });
  if (response.status === 404) throw new UploadGone('upload not found');
  // Uploads never replace an object: the key is taken (at start, or by the time it completed).
  if (response.status === 409) {
    const detail = (await response.text().catch(() => '')).trim();
    throw new KeyTaken(detail || 'an object already exists there');
  }
  // Anything else carries the server's message, or the status (a proxy's HTML error page is not
  // a message for people).
  if (!response.ok) throw await responseError(response, `POST ${path}`);
  return response;
}

/** `post`, and its answer's JSON body. */
async function postJson<T>(path: string, body: unknown): Promise<T> {
  return (await post(path, body)).json();
}

export const apiTransport: UploadTransport = {
  start(key, size, contentType, project) {
    return postJson<StartedUpload>(uploadPaths.start, {
      key,
      size,
      content_type: contentType,
      ...projectField(project),
    } satisfies Schemas['UploadStartRequest']);
  },
  async partUrls(uploadId, key, parts, project) {
    const res = await postJson<UploadPartUrlList>(uploadPaths.parts(uploadId), {
      key,
      part_numbers: parts,
      ...projectField(project),
    } satisfies Schemas['UploadPartsRequest']);
    return new Map(res.urls.map((u) => [u.part_number, u.url]));
  },
  async status(uploadId, key, project) {
    const res = await postJson<UploadStatus>(uploadPaths.status(uploadId), {
      key,
      ...projectField(project),
    } satisfies Schemas['UploadKeyRequest']);
    return new Map(res.parts.map((p) => [p.part_number, p.etag]));
  },
  complete(uploadId, key, parts, project) {
    return postJson<UploadedFile>(uploadPaths.complete(uploadId), {
      key,
      parts: parts.map(([part_number, etag]) => ({ part_number, etag })),
      ...projectField(project),
    } satisfies Schemas['UploadCompleteRequest']);
  },
  async abort(uploadId, key, project) {
    await post(uploadPaths.abort(uploadId), {
      key,
      ...projectField(project),
    } satisfies Schemas['UploadKeyRequest']);
  },
  async putPart(url, body, signal) {
    const response = await fetch(url, { method: 'PUT', body, signal });
    // Expired, mostly; the code S3 gives says so (or names another refusal, such as AccessDenied).
    if (response.status === 403) {
      const code = /<Code>([^<]+)<\/Code>/.exec(await response.text().catch(() => ''))?.[1];
      throw new PartUrlExpired(`S3 refused the part (403${code ? ` ${code}` : ''})`);
    }
    // S3's NoSuchUpload: the upload was aborted or cleaned up, so nothing can be sent to it.
    if (response.status === 404) throw new UploadGone('upload not found');
    if (!response.ok) throw new Error(`part upload failed: ${response.status}`);
    const etag = response.headers.get('etag');
    if (!etag)
      throw new Error(
        "the bucket's CORS rule does not expose ETag; an admin should check the upload settings",
      );
    return etag.replaceAll('"', '');
  },
  async landed(uri, size, project) {
    const [head] = await headDataObjects([uri], project);
    return head?.exists === true && head.size === size;
  },
};
