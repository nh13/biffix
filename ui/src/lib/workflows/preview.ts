// A form preview (`biffix workflows form --open`): an unregistered workflow's launch form, carried
// in the page URL's fragment as base64url(zlib(JSON)) so it never reaches the server.

import { base64UrlDecode } from '#lib/util/base64url.js';

/** What a preview URL carries. */
export interface FormPreview {
  name: string;
  engine: string;
  form_schema: unknown;
}

/**
 * The preview a fragment payload carries; throws when it isn't one. The payload is written by the
 * CLI (`crates/biffix-cli/src/commands/workflows.rs`), so there is no encoder here.
 */
export async function decodePreview(payload: string): Promise<FormPreview> {
  const deflated = base64UrlDecode(payload.replace(/^#/, '').trim());
  const json = new TextDecoder().decode(await pipe(deflated, new DecompressionStream('deflate')));
  const value = JSON.parse(json) as Partial<FormPreview> | null;
  const schema = value?.form_schema;
  // `typeof null` is 'object', so a null (or array) form_schema has to be refused explicitly.
  if (
    !value ||
    typeof value.name !== 'string' ||
    typeof schema !== 'object' ||
    schema === null ||
    Array.isArray(schema)
  ) {
    throw new Error('this link does not hold a form');
  }
  return { name: value.name, engine: String(value.engine ?? ''), form_schema: schema };
}

async function pipe(bytes: Uint8Array, stream: DecompressionStream) {
  const out = new Blob([bytes as BlobPart]).stream().pipeThrough(stream);
  return new Uint8Array(await new Response(out).arrayBuffer());
}
