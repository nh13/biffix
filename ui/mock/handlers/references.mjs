// Reference bundle routes (references.rs): list, add, replace and delete.
import { PROJECT_SOURCES } from '../fixtures/data.mjs';
import { REFERENCES } from '../fixtures/references.mjs';
import { readJson } from '../lib/http.mjs';
import { bytewise } from '../lib/paging.mjs';
import { bundleProblem, storedBundle } from '../lib/references.mjs';
import { ME_SUB } from '../state.mjs';

/** `GET /references?kind=&keys=`: the bundles of a kind that have every key. */
function listReferences({ query, send }) {
  const kind = query.get('kind');
  const keys = (query.get('keys') ?? '').split(',').filter(Boolean);
  send(
    200,
    REFERENCES.filter((b) => (!kind || b.kind === kind) && keys.every((k) => k in b.uris)),
  );
}

/** `POST /references` (`id` null) and `PUT /references/{id}`: add or replace a bundle. */
function saveReference({ req, send }, id) {
  readJson(req, send, (r) => {
    // As references.rs: an id change is refused first, then the bundle is checked (against every
    // project's sources: bundles are tenant-wide), and only then does the store find the id taken
    // (409) or the bundle to replace missing (404).
    if (req.method === 'PUT' && r.id !== id)
      return send(400, "a bundle's id cannot change; add a new bundle instead");
    const problem = bundleProblem(r, Object.values(PROJECT_SOURCES).flat());
    if (problem) return send(400, problem);
    const existing = REFERENCES.find((b) => b.id === r.id);
    if (req.method === 'POST' && existing)
      return send(409, `reference bundle ${r.id} already exists`);
    if (req.method === 'PUT' && !existing) return send(404, 'not found');
    // The store keeps who made a bundle and when.
    const bundle = {
      ...storedBundle(r, Date.now() * 1000, ME_SUB),
      ...(existing
        ? { created_at_micros: existing.created_at_micros, created_by: existing.created_by }
        : {}),
    };
    // Listed by id, bytewise (the in-memory store's BTreeMap; Postgres orders by id COLLATE "C").
    const next = [...REFERENCES.filter((b) => b.id !== r.id), bundle].sort((a, b) =>
      bytewise(a.id, b.id),
    );
    REFERENCES.splice(0, REFERENCES.length, ...next);
    // A path with "missing" in it stands for a file that is not there yet.
    const warnings = Object.entries(r.uris ?? {})
      .filter(([, u]) => u.includes('missing'))
      .map(([k, u]) => `${k}: nothing found at ${u}`);
    send(req.method === 'POST' ? 201 : 200, { bundle, warnings });
  });
}

/** `DELETE /references/{id}`. */
function deleteReference({ params, send }) {
  const at = REFERENCES.findIndex((b) => b.id === params.id);
  if (at < 0) return send(404, 'not found');
  REFERENCES.splice(at, 1);
  send(204);
}

export const REFERENCE_ROUTES = [
  ['GET', '/references', listReferences],
  ['POST', '/references', (c) => saveReference(c, null)],
  ['PUT', '/references/{id}', (c) => saveReference(c, c.params.id)],
  ['DELETE', '/references/{id}', deleteReference],
];
