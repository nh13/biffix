// A reference bundle write, checked as store/references.rs `validate_request` checks it, against
// the data sources it is given (the backend's: every project's, as bundles are tenant-wide).

// store/references.rs: the kinds a form can ask for, and the caps on a bundle's parts.
const BUNDLE_KINDS = ['assembly', 'assay_kit'];
const MAX_BUNDLE_URIS = 64;
const MAX_BUNDLE_URI_BYTES = 1100;
const MAX_BUNDLE_NOTES = 4000;

/** `DataSource::holds`: an object or folder under the source's root (or its root, named without
 * the trailing `/`). */
const holds = (source, uri) => uri.startsWith(source.uri) || `${uri}/`.startsWith(source.uri);
/** `DataSource::reserves`: under a prefix biffix keeps for its own files (or one of them named
 * without its trailing `/`). */
function reserves(source, uri) {
  if (!holds(source, uri)) return false;
  const path = uri.slice(source.uri.length);
  return (source.hidden_prefixes ?? []).some((p) => path.startsWith(p) || `${path}/`.startsWith(p));
}

/** The 400's message for a bundle `validate_request` refuses, else `null`. */
export function bundleProblem(r, sources) {
  const id = String(r.id ?? '');
  if (!/^[a-z0-9][a-z0-9._-]{0,63}$/.test(id))
    return `id \`${id}\` must be 1–64 lowercase letters, digits, \`-\`, \`_\` or \`.\`, starting with a letter or digit`;
  if (!BUNDLE_KINDS.includes(r.kind)) return `kind must be one of ${BUNDLE_KINDS.join(', ')}`;
  const name = String(r.display_name ?? '').trim();
  if (!name || [...name].length > 200) return 'display_name must be 1–200 characters';
  const uris = Object.entries(r.uris ?? {});
  if (!uris.length) return 'a bundle needs at least one URI';
  if (uris.length > MAX_BUNDLE_URIS)
    return `a bundle names at most ${MAX_BUNDLE_URIS} URIs (${uris.length} given)`;
  if (r.notes != null && [...r.notes].length > MAX_BUNDLE_NOTES)
    return `notes must be at most ${MAX_BUNDLE_NOTES} characters`;
  const genome = (r.igv_genome ?? '').trim();
  if (genome && !/^[A-Za-z0-9._-]{1,32}$/.test(genome))
    return `igv_genome \`${genome}\` must be 1–32 letters, digits, \`.\`, \`_\` or \`-\` (a genome id such as hg38 or mm39)`;
  for (const [key, uri] of uris) {
    if (!/^[a-z0-9_]{1,64}$/.test(key))
      return `key \`${key}\` must be 1–64 lowercase letters, digits and \`_\``;
    if (Buffer.byteLength(uri) > MAX_BUNDLE_URI_BYTES)
      return `${key}: the URI is longer than ${MAX_BUNDLE_URI_BYTES} bytes`;
    if (!/^s3:\/\/[^/]+\/./.test(uri)) return `${key}: \`${uri}\` is not an s3://bucket/key URI`;
    if (!sources.some((s) => holds(s, uri)))
      return `${key}: \`${uri}\` is not in the project bucket or a registered data source`;
    if (sources.some((s) => reserves(s, uri)))
      return `${key}: \`${uri}\` is under a prefix biffix reserves for its own files`;
  }
  return null;
}

/** references.rs `bundle_of`: the bundle as stored (name and genome trimmed, blank notes and
 * genome left out), stamped. */
export function storedBundle(r, stamp, createdBy) {
  const { notes, igv_genome, ...rest } = r;
  const genome = (igv_genome ?? '').trim();
  return {
    ...rest,
    display_name: r.display_name.trim(),
    ...(notes != null && notes.trim() ? { notes } : {}),
    ...(genome ? { igv_genome: genome } : {}),
    created_at_micros: stamp,
    updated_at_micros: stamp,
    created_by: createdBy,
  };
}
