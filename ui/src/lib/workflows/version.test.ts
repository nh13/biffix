import { describe, it, expect } from 'vitest';
import { groupVersions, versionLabel, versionLabelOfId } from './version';

describe('versionLabel', () => {
  it('distinguishes same-sha versions by their content hash', () => {
    const a = versionLabel({ id: 'smoke:v1:0', git_sha: 'smoke' });
    const b = versionLabel({ id: 'smoke:v4-imgfix:0', git_sha: 'smoke' });
    expect(a).toBe('smoke:v1:0');
    expect(b).toBe('smoke:v4-imgfix:0');
    expect(a).not.toBe(b);
  });
  it('shortens long shas and hashes', () => {
    expect(
      versionLabel({ id: '0123456789abcdef:fedcba9876543210:0', git_sha: '0123456789abcdef' }),
    ).toBe('0123456789:fedcba987654:0');
  });
  it('keeps the build, so two builds of one source are told apart', () => {
    const hash = 'ab'.repeat(32);
    const b0 = versionLabel({ id: `0123456789abcdef:${hash}:0`, git_sha: '0123456789abcdef' });
    const b1 = versionLabel({ id: `0123456789abcdef:${hash}:1`, git_sha: '0123456789abcdef' });
    expect(b0).toBe('0123456789:abababababab:0');
    expect(b1).toBe('0123456789:abababababab:1');
    expect(versionLabel({ id: 'abc:hash:12', git_sha: 'abc' })).toBe('abc:hash:12');
  });
});

describe('versionLabelOfId', () => {
  it('labels a bare id as versionLabel labels the version', () => {
    for (const [id, git_sha] of [
      ['aabbccddeeff00:v1:0', 'aabbccddeeff00'],
      ['0123456789abcdef:fedcba9876543210:3', '0123456789abcdef'],
      ['smoke:v4-imgfix:2', 'smoke'],
    ])
      expect(versionLabelOfId(id)).toBe(versionLabel({ id, git_sha }));
  });
  it('tells apart same-commit versions that differ in content or build', () => {
    const labels = ['abc:hashA:0', 'abc:hashB:0', 'abc:hashA:1'].map(versionLabelOfId);
    expect(new Set(labels).size).toBe(3);
  });
  it('shows an id without a git sha half whole', () => {
    expect(versionLabelOfId('deadbeefcafe')).toBe('deadbeefcafe');
    expect(versionLabelOfId(':hash')).toBe(':hash');
  });
});

describe('groupVersions', () => {
  const v = (project: string, name: string, created: number, runs = 0) => ({
    id: `${project}/${name}@${created}`,
    project,
    name,
    created_at_micros: created,
    run_count: runs,
  });

  it('keeps two projects with the same workflow name apart', () => {
    const groups = groupVersions([v('a', 'rnaseq', 2), v('b', 'rnaseq', 1)]);
    expect(groups.map((g) => [g.project, g.name])).toEqual([
      ['a', 'rnaseq'],
      ['b', 'rnaseq'],
    ]);
  });

  it('takes the newest version as the latest, whatever the list order', () => {
    const [g] = groupVersions([v('a', 'w', 1, 2), v('a', 'w', 3, 5), v('a', 'w', 2, 1)]);
    expect(g.latest.created_at_micros).toBe(3);
    expect(g.versions.map((x) => x.created_at_micros)).toEqual([3, 2, 1]);
    expect(g.runCount).toBe(8);
  });

  it('does not collide on a separator inside a name', () => {
    const groups = groupVersions([v('a b', 'c', 1), v('a', 'b c', 1)]);
    expect(groups).toHaveLength(2);
  });
});
