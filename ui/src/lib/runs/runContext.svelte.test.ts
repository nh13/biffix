import { describe, expect, it } from 'vitest';
import type { RunDetail } from '$lib/api';
import { RENAME_HOLD_MS, RunState } from './runContext.svelte';

const detail = (run_id: string, run_name: string | null) => ({ run_id, run_name }) as RunDetail;

describe('RunState.withRename', () => {
  it('keeps a name just saved over a poll that was in flight before it', () => {
    const ctx = new RunState();
    ctx.run = detail('r1', 'old');
    ctx.applyRename('r1', 'new', 1_000);
    expect(ctx.run.run_name).toBe('new');
    expect(ctx.withRename(detail('r1', 'old'), 2_000).run_name).toBe('new');
  });

  it('lets go once a poll returns the new name, so a later rename elsewhere shows', () => {
    const ctx = new RunState();
    ctx.applyRename('r1', 'new', 1_000);
    expect(ctx.withRename(detail('r1', ' new '), 2_000).run_name).toBe(' new ');
    expect(ctx.withRename(detail('r1', 'renamed elsewhere'), 3_000).run_name).toBe(
      'renamed elsewhere',
    );
  });

  it('lets go after the hold, and holds a cleared name too', () => {
    const ctx = new RunState();
    ctx.applyRename('r1', null, 1_000);
    expect(ctx.withRename(detail('r1', 'old'), 1_000 + RENAME_HOLD_MS).run_name).toBeNull();
    expect(ctx.withRename(detail('r1', 'old'), 2_000 + RENAME_HOLD_MS).run_name).toBe('old');
  });

  it('passes another run through untouched', () => {
    const ctx = new RunState();
    ctx.applyRename('r1', 'new', 1_000);
    const other = detail('r2', 'theirs');
    expect(ctx.withRename(other, 2_000)).toBe(other);
  });
});
