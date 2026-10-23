// Derive human link-outs from a frozen version's `source` + git_sha.
// The backend already records where the source came from, so we surface it rather than asking the
// registrant for a separate URL. For a git source on GitHub we can point at the exact frozen commit
// and its README; for any other host we only trust the repo root; an S3 source links the console.

import { s3ConsoleUrl } from '#lib/util/links.js';

export interface SourceLinks {
  /** Kind of source, for the block heading. */
  kind: 'git' | 's3';
  /** Short human label (repo path or S3 URI). */
  label: string;
  /** Repo root or ref tree (git). */
  repo?: string;
  /** The exact frozen commit (git on a known host only). */
  commit?: string;
  /** README at the frozen sha (git on a known host only). */
  readme?: string;
  /** S3 console (s3 only). */
  s3?: string;
}

/**
 * Normalize a git remote to an `https` base URL with no trailing `.git`. Handles the three shapes a
 * registration realistically carries: an https URL, an scp-style SSH remote from a dev box
 * (`git@github.com:owner/repo.git`), and a bare `owner/repo`. Returns null when it doesn't parse.
 */
export function normalizeGitRemote(
  repo: string,
): { host: string; path: string; base: string } | null {
  let r = repo.trim();
  if (!r) return null;
  r = r.replace(/\/+$/, '').replace(/\.git$/, '');
  // scp-style SSH: git@host:owner/repo
  const scp = /^[\w.-]+@([\w.-]+):\/?(.+)$/.exec(r);
  if (scp) {
    const host = scp[1];
    const path = scp[2].replace(/^\/+/, '');
    return { host, path, base: `https://${host}/${path}` };
  }
  // full URL: https://host/owner/repo, ssh://git@host/owner/repo, git://host/owner/repo
  const url = /^(?:https?|ssh|git):\/\/(?:[^@/]+@)?([\w.-]+)\/(.+)$/.exec(r);
  if (url) {
    return { host: url[1], path: url[2], base: `https://${url[1]}/${url[2]}` };
  }
  // bare owner/repo → assume GitHub (the house host)
  if (/^[\w.-]+\/[\w.-]+$/.test(r)) {
    return { host: 'github.com', path: r, base: `https://github.com/${r}` };
  }
  return null;
}

function isGithub(host: string): boolean {
  return host === 'github.com' || host.endsWith('.github.com');
}

/** Build the link-out set for a version's source. `source` is the opaque `serde_json::Value`. */
export function sourceLinks(source: unknown, gitSha?: string | null): SourceLinks | null {
  if (!source || typeof source !== 'object') return null;
  const s = source as Record<string, unknown>;

  if (s.kind === 'git' && typeof s.repo === 'string') {
    const g = normalizeGitRemote(s.repo);
    const ref = typeof s.git_ref === 'string' ? s.git_ref : undefined;
    if (!g) return { kind: 'git', label: s.repo };
    const gh = isGithub(g.host);
    const sha = gitSha || ref;
    return {
      kind: 'git',
      label: g.path,
      // On a known host, prefer the ref tree; otherwise only trust the repo root.
      repo: gh && ref ? `${g.base}/tree/${encodeURIComponent(ref)}` : g.base,
      commit: gh && gitSha ? `${g.base}/commit/${encodeURIComponent(gitSha)}` : undefined,
      readme: gh && sha ? `${g.base}/blob/${encodeURIComponent(sha)}/README.md` : undefined,
    };
  }

  if (s.kind === 's3' && typeof s.uri === 'string') {
    return { kind: 's3', label: s.uri, s3: s3ConsoleUrl(s.uri) ?? undefined };
  }

  return null;
}
