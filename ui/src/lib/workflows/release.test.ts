import { describe, expect, it } from 'vitest';
import { hasRelease, isReleased, launchVersion, releaseFilter, releaseLabel } from './release';
import { groupVersions } from './version';

const v = (name: string, id: string, at: number, released?: boolean | null) => ({
  id,
  name,
  project: 'p',
  created_at_micros: at,
  ...(released === undefined ? {} : { release: { released } }),
});

describe('released versions', () => {
  it('reads only an explicit true as released', () => {
    expect(isReleased(v('a', '1', 1, true))).toBe(true);
    expect(isReleased(v('a', '1', 1, false))).toBe(false);
    expect(isReleased(v('a', '1', 1, null))).toBe(false);
    expect(releaseLabel(v('a', '1', 1, true))).toBe('Released');
    expect(releaseLabel(v('a', '1', 1))).toBe('Developer');
  });

  it('launches the newest released version, else the newest', () => {
    const [align] = groupVersions([
      v('align', 'new', 3, false),
      v('align', 'old', 1, true),
      v('align', 'mid', 2, true),
    ]);
    expect(align.latest.id).toBe('new');
    expect(hasRelease(align)).toBe(true);
    expect(launchVersion(align).id).toBe('mid');
    const [dev] = groupVersions([v('dev', 'd1', 1), v('dev', 'd2', 2, false)]);
    expect(hasRelease(dev)).toBe(false);
    expect(launchVersion(dev).id).toBe('d2');
  });

  it('shows pipelines with a release by default, and every one with Developer on', () => {
    const groups = groupVersions([
      v('align', 'a1', 2, true),
      v('align', 'a2', 3),
      v('call', 'c1', 1),
      v('qc', 'q1', 1, true),
    ]);
    const released = releaseFilter(groups, false);
    expect(released.shown.map((g) => g.name)).toEqual(['align', 'qc']);
    expect(released.hiddenByFilter).toBe(1);
    const all = releaseFilter(groups, true);
    expect(all.shown.map((g) => g.name)).toEqual(['align', 'call', 'qc']);
    expect(all.hiddenByFilter).toBe(0);
    expect(releaseFilter([], false)).toEqual({ shown: [], hiddenByFilter: 0 });
  });
});
