<script lang="ts">
  // Under a resumed analysis's status: it continues another one, linked, with how that one
  // ended, how many steps it reused instead of running again, and that its results went to the
  // same folder as the original's (when they share one).
  import type { RunDetail } from '$lib/api';
  import { runTabHref } from '$lib/runs/runNav';
  import { plural } from '$lib/util/format';
  import { runName, shortDay } from '$lib/runs/runName';
  import { sharesOriginFolder } from './inputs';

  interface Props {
    run: RunDetail;
    /** The analysis it continues, once loaded (`null`: unknown, or not readable). */
    origin: RunDetail | null;
  }
  let { run, origin }: Props = $props();

  const originId = $derived(run.resumed_from ?? '');
  const originName = $derived(origin ? runName(origin) : originId);
  const ended = $derived.by(() => {
    if (!origin) return '';
    const words: Record<string, string> = {
      succeeded: 'finished',
      failed: 'stopped with a problem',
      cancelled: 'cancelled',
    };
    const word = Object.hasOwn(words, origin.state) ? words[origin.state] : null;
    if (!word) return '';
    // Dated by when it ended only: its submit day is not when it finished.
    const at = origin.completed_at;
    return at != null ? `${word} ${shortDay(at)}` : word;
  });
  const reused = $derived(run.tasks_cached ?? 0);
  const shared = $derived(sharesOriginFolder(run, origin));
</script>

{#if originId}
  <p class="resumed" data-testid="resumed-note">
    <span aria-hidden="true">↻</span>
    Continues
    <a href={runTabHref(originId, 'overview')}>{originName}</a
    >{#if ended}{' '}({ended}){/if}{#if reused > 0}{' · '}{plural(reused, 'step')}
      reused{/if}{#if shared}{' · '}results written to the same folder{/if}
  </p>
{/if}

<style>
  .resumed {
    margin: -0.6rem 0 1.25rem;
    font-size: 0.85rem;
    color: var(--text-muted);
  }
  .resumed a {
    color: var(--accent);
    text-decoration: underline;
  }
</style>
