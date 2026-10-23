// The Data page's and the file picker's routes: the deploy's projects, each project's data
// sources, and a source's folders and files (as data.rs and data_sources.rs answer).
import { DATA_OBJECTS, PROJECTS, PROJECT_SOURCES } from '../fixtures/data.mjs';
import { OUTPUTS } from '../fixtures/outputs.mjs';
import { now } from '../lib/clock.mjs';
import { jsonToParams, readJson } from '../lib/http.mjs';
import { downloadLink, previewBody } from '../lib/object-reads.mjs';
import { badLimit, bytewise, clampInt } from '../lib/paging.mjs';
import { viewerOf } from '../lib/viewer.mjs';

/**
 * api::request_project: the project a request names (the default when it names none), or the 400
 * message for one this deploy does not configure.
 */
export function requestProject(named) {
  const id = typeof named === 'string' ? named.trim() : '';
  if (!id) return { project: PROJECTS.find((p) => p.default) };
  const project = PROJECTS.find((p) => p.id === id);
  if (project) return { project };
  const ids = PROJECTS.map((p) => p.id).join(', ');
  return { error: `project \`${id}\` is not configured on this deploy (projects: ${ids})` };
}
/** A project's data sources: its bucket first, writable when it allows uploads (as `view`). */
const sourcesOf = (project) => PROJECT_SOURCES[project.id];
/** The sources a data read names the project of, or `null` once it answered the 400. */
function sourcesFor(args, send) {
  const { project, error } = requestProject(args.get('project'));
  if (error) {
    send(400, error);
    return null;
  }
  return sourcesOf(project);
}

// One `/data/list` page: S3's delimited listing of up to 1,000 entries (folders and files together,
// in key order), with a cursor for the next page (data.rs LIST_PAGE).
const LIST_PAGE = 1000;

// data_sources.rs `DataSource::hides`, from the rule the source itself carries
// (`hidden_prefixes` except `browsable_exceptions`).
const hides = (source, path) =>
  (source.hidden_prefixes ?? []).some((p) => path.startsWith(p)) &&
  !(source.browsable_exceptions ?? []).some((p) => path.startsWith(p));
/** What the data reads see, by URI: `DATA_OBJECTS`, and every run's registered outputs still in S3, each
 * object once (a resumed run lists the outputs it reused, and S3 holds one object per key). */
function dataObjects() {
  const objects = new Map(DATA_OBJECTS.map((o) => [o.uri, o]));
  for (const o of Object.values(OUTPUTS).flat())
    if (o.exists !== false && !objects.has(o.s3_uri))
      objects.set(o.s3_uri, { uri: o.s3_uri, size: o.size ?? 0 });
  return objects;
}
// data.rs: `/data/find`'s default and largest limit, and the most URIs one `/data/head` names.
const FIND_DEFAULT = 500;
const FIND_MAX = 5000;
const HEAD_MAX = 200;

// The source biffix has no grant on (`locked`): every read of it is S3's AccessDenied, as data.rs
// words it.
const lockedMessage = (source) =>
  `biffix cannot read ${source.name} (${source.uri}); an admin should check its grant in the deploy config`;

/** The objects a `/data/list` or `/data/find` sees under its folder, with its source, root and
 * folder; `null` once it answered a refusal. */
function listedKeys(args, send) {
  const refuse = (status, message) => {
    send(status, message);
    return null;
  };
  const sources = sourcesFor(args, send);
  if (!sources) return null;
  const source = sources.find((s) => s.id === args.get('source'));
  if (!source) return refuse(404, 'unknown data source');
  if (source.id === 'locked') return refuse(403, lockedMessage(source));
  const root = source.uri;
  const path = args.get('path') ?? '';
  if (hides(source, path)) return refuse(404, `no folder \`${path}\``);
  const base = `${root}${path}`;
  // A hidden key is never listed, and the root (nor a search of it) never reaches into a hidden
  // folder, as data.rs reads them.
  const keys = [...dataObjects().values()].filter((o) => {
    if (!o.uri.startsWith(base)) return false;
    const rel = o.uri.slice(root.length);
    const top = rel.slice(0, rel.indexOf('/') + 1);
    return !hides(source, rel) && !(path === '' && top && hides(source, top));
  });
  return { source, root, path, base, keys };
}

/** `/data/find`, as `find_files`: the text trimmed and any case, in key order, at most `limit`
 * (`truncated` when a match was left out). */
function dataFind(args, send) {
  const listed = listedKeys(args, send);
  if (!listed) return;
  const { source, root, keys } = listed;
  const q = (args.get('q') ?? '').trim().toLowerCase();
  const limit = clampInt(args.get('limit'), FIND_DEFAULT, 1, FIND_MAX);
  if (limit == null) return send(...badLimit(args.get('limit'), true));
  const matches = keys
    .filter((o) => o.uri.split('/').pop().toLowerCase().includes(q))
    .sort((a, b) => bytewise(a.uri, b.uri));
  const files = matches.slice(0, limit).map((o) => dataFile(root, o));
  send(200, { source: source.id, files, truncated: matches.length > limit });
}

/** `/data/list`: one page of a folder's entries. */
function dataList(args, send) {
  const listed = listedKeys(args, send);
  if (!listed) return;
  const { source, root, path, base, keys } = listed;
  const starts = args.get('starts_with') ?? '';
  const entries = new Map();
  for (const o of keys) {
    const rest = o.uri.slice(base.length);
    if (!rest.startsWith(starts)) continue;
    const i = rest.indexOf('/');
    if (i >= 0) {
      const name = rest.slice(0, i);
      entries.set(`${name}/`, { folder: { name, path: `${path}${name}/` } });
    } else entries.set(rest, { file: dataFile(root, o) });
  }
  const cursor = args.get('cursor');
  const after = [...entries.keys()].sort(bytewise).filter((k) => !cursor || k > cursor);
  const page = after.slice(0, LIST_PAGE);
  const rows = page.map((k) => entries.get(k));
  return send(200, {
    source: source.id,
    path,
    folders: rows.flatMap((e) => (e.folder ? [e.folder] : [])),
    files: rows.flatMap((e) => (e.file ? [e.file] : [])),
    ...(after.length > page.length ? { next_cursor: page[page.length - 1] } : {}),
  });
}

/** One file of a data source: preview, inline view or download link. */
function dataFileRead(op, args, send) {
  const sources = sourcesFor(args, send);
  if (!sources) return;
  const source = sources.find((s) => s.id === args.get('source'));
  if (!source) return send(404, 'unknown data source');
  // data.rs `file_uri`: a path that cannot name a file is a 400; a hidden one is not found; then
  // S3 is read, which the locked source refuses (as its listing is refused).
  const given = args.get('path') ?? '';
  const path = given.replace(/^\/+/, '');
  if (!path || path.endsWith('/') || path.split('/').includes('..'))
    return send(400, `\`${given}\` is not a file path`);
  if (hides(source, path)) return send(404, 'no such file');
  if (source.id === 'locked') return send(403, lockedMessage(source));
  const o = dataObjects().get(`${source.uri}${path}`);
  if (!o) return send(404, 'no such file');
  const name = path.split('/').pop();
  if (op === 'preview') {
    const text = name.endsWith('.tsv')
      ? 'chrom\tlength\tgc\nchr1\t248956422\t0.41\nchr2\t242193529\t0.40\n'
      : name.endsWith('.fa')
        ? '>chr1\nNNNNNNNNNNACGTACGTACGT\n>chr2\nNNNNNNNNNNTTGACCATGA\n'
        : null;
    return send(200, previewBody(text));
  }
  if (op === 'view-url') {
    if (!['image', 'pdf', 'html'].includes(viewerOf(path)))
      return send(409, `${name} cannot be shown in the browser; download it`);
    return send(200, {
      url:
        'data:image/svg+xml,' +
        encodeURIComponent(
          '<svg xmlns="http://www.w3.org/2000/svg" width="120" height="40"><rect width="120" height="40" fill="#cde"/></svg>',
        ),
      expires_at: Date.now() / 1000 + 600,
      filename: name,
      content_type: 'image/svg+xml',
    });
  }
  return send(200, downloadLink(name));
}

function dataFile(root, o) {
  const path = o.uri.slice(root.length);
  return {
    name: path.split('/').pop(),
    path,
    uri: o.uri,
    size: o.size,
    last_modified_micros: (now - 2 * 86400) * 1_000_000,
    etag: 'mock',
    viewer: viewerOf(path),
  };
}

/** `GET /projects`. */
function listProjects({ send }) {
  send(200, PROJECTS);
}

/** `GET /data-sources?project=`: the file picker's sources — a project bucket, a sequencing-core
 * drop and a source the deploy cannot read (answers 403 like a missing grant). */
function listSources({ query, send }) {
  const sources = sourcesFor(query, send);
  if (sources) send(200, sources);
}

/** `DataSource::shows`: an object (not a folder) the source holds and does not hide. */
const shows = (source, uri) =>
  uri.startsWith(source.uri) && !uri.endsWith('/') && !hides(source, uri.slice(source.uri.length));

/** `POST /data/head`: which of some URIs exist, and their sizes. As data.rs: at most `HEAD_MAX`,
 * and a URI none of the project's sources shows is unknown (no `exists`), not absent. */
function headObjects({ req, send }) {
  readJson(req, send, ({ uris = [], project }) => {
    if (uris.length > HEAD_MAX) return send(400, `at most ${HEAD_MAX} uris per request`);
    const { project: named, error } = requestProject(project);
    if (error) return send(400, error);
    const sources = sourcesOf(named);
    const objects = dataObjects();
    send(200, {
      objects: uris.map((uri) => {
        if (!sources.some((s) => shows(s, uri))) return { uri };
        const o = objects.get(uri);
        return o ? { uri, exists: true, size: o.size } : { uri, exists: false };
      }),
    });
  });
}

/** A data read (`list`, `find`, `preview`, …), its arguments in its POST's JSON body (which keeps
 * folder and file names out of the request URL). */
const dataRead =
  (answer) =>
  ({ req, send }) =>
    readJson(req, send, (body) => answer(jsonToParams(body), send));

export const DATA_ROUTES = [
  ['GET', '/projects', listProjects],
  ['GET', '/data-sources', listSources],
  ['POST', '/data/list', dataRead(dataList)],
  ['POST', '/data/find', dataRead(dataFind)],
  ['POST', '/data/head', headObjects],
  ...['preview', 'view-url', 'download-url'].map((op) => [
    'POST',
    `/data/${op}`,
    dataRead((args, send) => dataFileRead(op, args, send)),
  ]),
];
