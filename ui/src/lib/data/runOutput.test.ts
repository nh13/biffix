import { describe, expect, it, vi } from 'vitest';
import type { RunOutputPage, RunOutput, listOutputs } from '$lib/api';
import { runOutputAt } from './runOutput';

const output = (s3_uri: string) => ({ output_id: s3_uri, s3_uri }) as unknown as RunOutput;
const page = (uris: string[], next: string | null, total = 0) =>
  ({ outputs: uris.map(output), next_cursor: next, total }) as unknown as RunOutputPage;

const BASE = 's3://b/biffix/runs/r1/';

describe('runOutputAt', () => {
  it("asks by the file's own path, so the file is on the first page", async () => {
    const list = vi.fn(async () => page([`${BASE}a/b/c.bam`, `${BASE}a/b/c.bam.bai`], null));
    const found = await runOutputAt(
      'r1',
      'a/b/c.bam',
      `${BASE}a/b/c.bam`,
      list as typeof listOutputs,
    );
    expect(found).toEqual({ output: output(`${BASE}a/b/c.bam`) });
    expect(list).toHaveBeenCalledTimes(1);
    expect(list.mock.calls[0]).toEqual([
      'r1',
      expect.objectContaining({ prefix: 'a/b/c.bam', roles: ['*'] }),
    ]);
  });

  it('pages on to a match on a later page, and stops there', async () => {
    const list = vi
      .fn()
      .mockResolvedValueOnce(page([`${BASE}x.html1`], 'c1'))
      .mockResolvedValueOnce(page([`${BASE}x.html`], 'c2'))
      .mockResolvedValue(page([], null));
    const found = await runOutputAt('r1', 'x.html', `${BASE}x.html`, list as typeof listOutputs);
    expect(found).toEqual({ output: output(`${BASE}x.html`) });
    expect(list).toHaveBeenCalledTimes(2);
    expect(list.mock.calls[1][1]).toMatchObject({ after: 'c1' });
  });

  it('says the run recorded none there only when every page was read', async () => {
    const list = vi.fn(async () => page([`${BASE}other.bam`], null));
    expect(await runOutputAt('r1', 'c.bam', `${BASE}c.bam`, list as typeof listOutputs)).toEqual({
      notRecorded: true,
    });
  });

  it('says it gave up, not "not recorded", when it hits its cap with more to read', async () => {
    let n = 0;
    const list = vi.fn(async () =>
      page(
        Array.from({ length: 200 }, () => `${BASE}c.bam${n++}`),
        `c${n}`,
      ),
    );
    expect(await runOutputAt('r1', 'c.bam', `${BASE}c.bam`, list as typeof listOutputs)).toEqual({
      truncated: true,
    });
    expect(list).toHaveBeenCalledTimes(10);
  });

  it('rejects when the outputs cannot be read', async () => {
    const list = vi.fn(async () => {
      throw new Error('down');
    });
    await expect(
      runOutputAt('r1', 'c.bam', `${BASE}c.bam`, list as typeof listOutputs),
    ).rejects.toThrow('down');
  });
});
