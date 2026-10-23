import { describe, expect, it } from 'vitest';
import { deflateSync } from 'node:zlib';
import { decodePreview } from './preview';

const preview = {
  name: 'hello',
  engine: 'nextflow',
  form_schema: { fields: [{ name: 'greeting', widget: 'text', required: true }], warnings: [] },
};

/** What the CLI writes after `/workflows/preview#`: base64url(zlib(JSON)) without padding. */
const encode = (value: unknown): string =>
  deflateSync(Buffer.from(JSON.stringify(value))).toString('base64url');

describe('form preview payload', () => {
  it('reads what the CLI writes (zlib, base64url without padding)', async () => {
    const payload = encode(preview);
    expect(payload).toMatch(/^[A-Za-z0-9_-]+$/);
    expect(await decodePreview(payload)).toEqual(preview);
    expect(await decodePreview(`#${payload}`)).toEqual(preview);
  });

  it('refuses a payload that is not a form', async () => {
    await expect(decodePreview('not-a-form')).rejects.toThrow();
    await expect(decodePreview(encode({ name: 1 }))).rejects.toThrow('does not hold a form');
    await expect(decodePreview(encode(null))).rejects.toThrow('does not hold a form');
  });

  it('refuses a named payload whose form_schema is missing, null or not an object', async () => {
    // The CLI always writes a form_schema object, so anything else is a broken link, not an empty
    // form.
    for (const form_schema of [undefined, null, 'x', 3, ['a']]) {
      await expect(decodePreview(encode({ name: 'x', engine: 'e', form_schema }))).rejects.toThrow(
        'does not hold a form',
      );
    }
  });
});
