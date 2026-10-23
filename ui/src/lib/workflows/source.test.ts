import { describe, it, expect } from 'vitest';
import { normalizeGitRemote, sourceLinks } from './source';

describe('normalizeGitRemote', () => {
  it('normalizes an https URL and strips .git', () => {
    expect(normalizeGitRemote('https://github.com/example/foo.git')).toEqual({
      host: 'github.com',
      path: 'example/foo',
      base: 'https://github.com/example/foo',
    });
    // A trailing slash (after `.git` too) would double the links' `/`.
    for (const remote of ['https://github.com/example/foo/', 'https://github.com/example/foo.git/'])
      expect(normalizeGitRemote(remote)?.base, remote).toBe('https://github.com/example/foo');
  });

  it('rewrites an scp-style SSH remote to https', () => {
    expect(normalizeGitRemote('git@github.com:example/foo.git')).toEqual({
      host: 'github.com',
      path: 'example/foo',
      base: 'https://github.com/example/foo',
    });
  });

  it('treats a bare owner/repo as GitHub', () => {
    expect(normalizeGitRemote('example/foo')?.base).toBe('https://github.com/example/foo');
  });

  it('handles a non-github host', () => {
    expect(normalizeGitRemote('https://gitlab.com/acme/bar')?.host).toBe('gitlab.com');
  });

  it('returns null for junk', () => {
    expect(normalizeGitRemote('   ')).toBeNull();
    expect(normalizeGitRemote('not a repo at all')).toBeNull();
  });
});

describe('sourceLinks', () => {
  it('builds commit + README links for a GitHub git source', () => {
    const l = sourceLinks(
      { kind: 'git', repo: 'git@github.com:example/foo.git', git_ref: 'v2.0.1' },
      '94f2cf0',
    );
    expect(l?.kind).toBe('git');
    expect(l?.repo).toBe('https://github.com/example/foo/tree/v2.0.1');
    expect(l?.commit).toBe('https://github.com/example/foo/commit/94f2cf0');
    expect(l?.readme).toBe('https://github.com/example/foo/blob/94f2cf0/README.md');
  });

  it('omits commit/README for a non-github host, keeping the repo root', () => {
    const l = sourceLinks(
      { kind: 'git', repo: 'https://gitlab.com/acme/bar', git_ref: 'main' },
      'abc',
    );
    expect(l?.repo).toBe('https://gitlab.com/acme/bar');
    expect(l?.commit).toBeUndefined();
    expect(l?.readme).toBeUndefined();
  });

  it('links the S3 console for an s3 source', () => {
    const l = sourceLinks({ kind: 's3', uri: 's3://bucket/prefix/' }, null);
    expect(l?.kind).toBe('s3');
    expect(l?.s3).toContain('s3.console.aws.amazon.com');
  });

  it('returns null for a malformed source', () => {
    expect(sourceLinks(null, null)).toBeNull();
    expect(sourceLinks({ kind: 'git' }, null)).toBeNull();
  });
});
