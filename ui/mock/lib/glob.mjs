// Whether a relative `path` matches a declared report's glob (biffix-workflow glob.rs): `*`
// within one path segment, `**` across segments (`**/` also across none), `?` one character.
const TOKENS = new Map([
  ['**/', '(?:.*/)?'],
  ['**', '.*'],
  ['*', '[^/]*'],
  ['?', '[^/]'],
]);
// Each glob compiled once: a listing matches every output against the same few globs.
const COMPILED = new Map();
export function globMatches(pattern, path) {
  let re = COMPILED.get(pattern);
  if (!re) {
    const body = pattern
      .split(/(\*\*\/|\*\*|\*|\?)/)
      .map((part) => TOKENS.get(part) ?? part.replace(/[.+^${}()|[\]\\]/g, '\\$&'))
      .join('');
    re = new RegExp(`^${body}$`);
    COMPILED.set(pattern, re);
  }
  return re.test(path);
}
