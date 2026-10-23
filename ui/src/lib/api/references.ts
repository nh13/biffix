// Reference bundles: named sets of reference files a launch form's reference dropdown fills params
// from. Any viewer lists them; admins manage them.

import type { components } from '#lib/generated/api.js';
import { API_PATHS, get, pathFor, request, requestJson, withQuery, type JsonBody } from './client';

export type ReferenceBundle = JsonBody<'list_references'>[number];
export type ReferenceBundleRequest = components['schemas']['ReferenceBundleRequest'];
export type SavedReferenceBundle = components['schemas']['SavedReferenceBundle'];

/** Reference bundles, narrowed to `kind` and to those holding every one of `keys`. */
export function listReferences(filter: { kind?: string | null; keys?: string[] } = {}) {
  const q = new URLSearchParams();
  if (filter.kind) q.set('kind', filter.kind);
  if (filter.keys?.length) q.set('keys', filter.keys.join(','));
  return get<ReferenceBundle[]>(withQuery(API_PATHS.references, q));
}

const ADMIN_ONLY = 'Requires admin access';

/** Add a bundle (`id` new) or replace one (`id` existing, `update`). */
export function saveReference(
  bundle: ReferenceBundleRequest,
  update: boolean,
): Promise<SavedReferenceBundle> {
  const path = update ? pathFor(API_PATHS.reference, { id: bundle.id }) : API_PATHS.references;
  return requestJson<SavedReferenceBundle>(path, {
    method: update ? 'PUT' : 'POST',
    body: bundle,
    forbidden: ADMIN_ONLY,
  });
}

export async function deleteReference(id: string): Promise<void> {
  await request(pathFor(API_PATHS.reference, { id }), { method: 'DELETE', forbidden: ADMIN_ONLY });
}
