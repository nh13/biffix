import { describe, expect, it } from 'vitest';
import { UrlEcho } from './urlEcho';

describe('UrlEcho', () => {
  it("skips the URL's echo of a report, and takes any other change", () => {
    const echo = new UrlEcho();
    echo.reported('ali', '');
    expect(echo.isEcho('ali')).toBe(true);
    // Back/forward to an older value: not an echo.
    expect(echo.isEcho('')).toBe(false);
  });

  it('expects no echo for a report the URL already holds', () => {
    const echo = new UrlEcho();
    echo.reported('ali', 'ali');
    expect(echo.isEcho('ali')).toBe(false);
  });

  it('consumes reports an echo overtook', () => {
    const echo = new UrlEcho();
    echo.reported('a', '');
    echo.reported('ab', 'a');
    expect(echo.isEcho('ab')).toBe(true);
    expect(echo.isEcho('a')).toBe(false);
  });

  it('forgets pending reports on an outside change', () => {
    const echo = new UrlEcho();
    echo.reported('a', '');
    expect(echo.isEcho('z')).toBe(false);
    expect(echo.isEcho('a')).toBe(false);
  });
});
