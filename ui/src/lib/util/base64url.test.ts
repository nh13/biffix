import { describe, expect, it } from 'vitest';
import { base64UrlDecode, base64UrlEncode } from './base64url';

describe('base64url', () => {
  it('round-trips bytes, with no padding and the URL-safe alphabet', () => {
    const bytes = Uint8Array.from([0xfb, 0xff, 0xbf, 0x00, 0x41]);
    const text = base64UrlEncode(bytes);
    expect(text).toBe('-_-_AEE');
    expect(base64UrlDecode(text)).toEqual(bytes);
    expect(base64UrlDecode('-_-_AEE=')).toEqual(bytes);
  });
});
