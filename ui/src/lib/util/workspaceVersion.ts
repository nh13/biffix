/**
 * The version in a Cargo.toml's `[workspace.package]` table: the release version every biffix
 * crate shares. vite.config.ts builds it into the UI, whose API docs page shows it (the committed
 * OpenAPI document has a fixed version, so a release need not commit a new copy).
 *
 * Reads only the table's own `version = "…"` line, so it takes blank lines, comments, CRLF line
 * endings and spacing around `=`, but not a multi-line or inherited value.
 */
export function workspaceVersion(cargoToml: string): string {
  let inTable = false;
  for (const raw of cargoToml.split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('[')) {
      inTable = line === '[workspace.package]';
      continue;
    }
    const version = inTable ? /^version\s*=\s*"([^"]+)"/.exec(line)?.[1] : undefined;
    if (version) return version;
  }
  throw new Error('no version in [workspace.package] of Cargo.toml');
}
