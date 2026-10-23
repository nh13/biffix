import { describe, expect, it } from 'vitest';
import { localDate } from '$lib/util/date';
import { versionLabel } from '$lib/workflows/version';
import { runVersionHref, runVersionLabel } from './runVersion';

describe('runVersionLabel', () => {
  it('names the version as its own page does, and the day it was registered', () => {
    const at = Date.UTC(2026, 9, 3, 12) * 1000;
    const id = 'aabbccddeeff00:v1';
    expect(runVersionLabel({ workflow: id, workflow_registered_at_micros: at })).toBe(
      `version ${versionLabel({ id, git_sha: 'aabbccddeeff00' })} · registered ${localDate(new Date(at / 1000))}`,
    );
    expect(runVersionLabel({ workflow: id, workflow_registered_at_micros: at })).toContain(
      'version aabbccddee:v1 · ',
    );
  });

  it('tells apart two versions of one commit registered the same day', () => {
    const at = Date.UTC(2026, 9, 3, 12) * 1000;
    const label = (workflow: string) =>
      runVersionLabel({ workflow, workflow_registered_at_micros: at });
    const hash = (c: string) => c.repeat(64);
    // Other content, or another build of the same source.
    const labels = [
      `1a2b3c4d5e:${hash('a')}:0`,
      `1a2b3c4d5e:${hash('b')}:0`,
      `1a2b3c4d5e:${hash('a')}:1`,
    ].map(label);
    expect(new Set(labels).size).toBe(3);
  });

  it('names the version alone when the registration date is unknown', () => {
    expect(runVersionLabel({ workflow: 'c4d5e6f7a8b9:v1' })).toBe('version c4d5e6f7a8:v1');
    expect(runVersionLabel({ workflow: 'abc:hash:2', workflow_registered_at_micros: null })).toBe(
      'version abc:hash:2',
    );
  });

  it('shows an id with no git sha half whole, and is null without a version', () => {
    expect(runVersionLabel({ workflow: 'deadbeefcafe' })).toBe('version deadbeefcafe');
    expect(runVersionLabel({ workflow: null })).toBeNull();
    expect(runVersionLabel({})).toBeNull();
  });
});

describe('runVersionHref', () => {
  it('links the exact version', () => {
    expect(runVersionHref({ workflow: 'c4d5e6f7a8b9:v1' })).toBe('/workflows/c4d5e6f7a8b9%3Av1');
    expect(runVersionHref({ workflow: null })).toBeNull();
  });
});
