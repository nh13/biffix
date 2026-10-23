<script lang="ts">
  import { onMount, untrack } from 'svelte';
  import { LogStreamModel, type FetchLogPage } from './logStreamModel.svelte';
  import { prefs } from '$lib/state/localPrefs.svelte';

  // An in-app CloudWatch log viewer, served through biffix (no AWS account needed). Opens at the tail
  // (or, toggled, at the start of the stream), pages on demand, and live-tails while `active`. The
  // paging and sequencing rules live in LogStreamModel; this is the view and the poll wiring.
  interface Props {
    /** Fetch one page of the stream; the caller binds the route (task, attempt, coordinator). */
    fetchPage: FetchLogPage;
    /** Whether the stream may still grow — drives live-tail polling. */
    active?: boolean;
    /** Live-tail poll interval in milliseconds. */
    pollMs?: number;
    /** Shown when there is no stream (yet). */
    emptyMessage?: string;
  }
  let {
    fetchPage,
    active = false,
    pollMs = 3000,
    emptyMessage = "No logs yet — this task hasn't started running.",
  }: Props = $props();

  const log = new LogStreamModel((opts) => fetchPage(opts));

  onMount(() => {
    void log.loadFirst();
    return () => log.dispose();
  });

  // While active: tick (the first page until a stream exists, then the tail). When active turns
  // false, fetch once more so the final lines (usually the error) show up.
  let wasActive = false;
  $effect(() => {
    if (active) {
      wasActive = true;
      // Like startPoll: no ticks while the tab is hidden, one as soon as it is shown again.
      const tick = () => {
        if (document.visibilityState !== 'hidden') void log.tick();
      };
      const timer = setInterval(tick, pollMs);
      document.addEventListener('visibilitychange', tick);
      return () => {
        clearInterval(timer);
        document.removeEventListener('visibilitychange', tick);
      };
    }
    if (wasActive) {
      wasActive = false;
      return untrack(() => log.settle(pollMs));
    }
  });

  const fmtTs = (ms: number) =>
    new Date(ms).toLocaleTimeString([], { hourCycle: 'h23' }) +
    '.' +
    String(ms % 1000).padStart(3, '0');

  // Display order is a per-viewer preference (Settings → Display). `log.lines` is always kept
  // oldest-first (the order CloudWatch pages arrive in); only the rendering flips. The copy is
  // bounded by the model's line cap, and the keyed each moves rows rather than rewriting them.
  const newestFirst = $derived(prefs.logOrder === 'newest');
  const shown = $derived(newestFirst ? log.lines.slice().reverse() : log.lines);
</script>

{#snippet earlier()}
  {#if log.headTrimmed}
    <span class="hint">— older lines trimmed (showing the newest {log.maxLines}) —</span>
  {:else if log.atStart}
    <span class="hint">— start of stream —</span>
  {:else}
    <button class="linkish" onclick={() => log.loadEarlier()} disabled={log.loadingEarlier}>
      {log.loadingEarlier ? 'Loading…' : 'Load earlier'}
    </button>
    {#if log.earlierError}
      <span class="hint err">Couldn't load earlier lines: {log.earlierError}</span>
    {/if}
  {/if}
{/snippet}

<div class="logs">
  {#if log.loading}
    <p class="hint">Loading logs…</p>
  {:else if log.error}
    <p class="hint err">Couldn't load logs: {log.error}</p>
  {:else if log.noStream}
    <p class="hint">{emptyMessage}</p>
  {:else if log.lines.length === 0}
    <p class="hint">No log events.</p>
  {:else}
    <div class="toolbar">
      <!-- "Load earlier" sits at the end where the older entries are: the top when oldest-first,
           the bottom when newest-first. -->
      {#if !newestFirst}{@render earlier()}{/if}
      <button class="linkish" onclick={() => log.toggleStart()}>
        {log.fromStart ? 'Jump to end' : 'From start'}
      </button>
      {#if log.tailCapped}
        <span class="hint">showing the first {log.maxLines} lines</span>
      {:else if log.fromStart && !active}
        <button class="linkish" onclick={() => log.loadMore()} disabled={log.loadingMore}>
          {log.loadingMore ? 'Loading…' : 'Load more'}
        </button>
      {/if}
      {#if active}<span class="live">● live</span>{/if}
      {#if newestFirst}<span class="hint">newest first</span>{/if}
    </div>
    <!-- A <div>, not a <pre>: inter-element source whitespace collapses (no spurious blank lines
         between entries); per-line `white-space: pre-wrap` preserves spacing WITHIN a message. -->
    <div class="stream">
      {#each shown as l (l.id)}
        <div class="ln"><span class="ts">{fmtTs(l.ts_ms)}</span> {l.message}</div>
      {/each}
    </div>
    {#if newestFirst}<div class="toolbar bottom">{@render earlier()}</div>{/if}
  {/if}
</div>

<style>
  .logs {
    margin: 0.25rem 0 0.5rem;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.75rem;
    margin-bottom: 0.25rem;
  }
  .toolbar.bottom {
    margin: 0.25rem 0 0;
  }
  .stream {
    max-height: 22rem;
    overflow: auto;
    margin: 0;
    padding: 0.5rem 0.6rem;
    background: var(--bg-content, #0d1117);
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    font-family: var(--mono, monospace);
    font-size: 0.72rem;
    line-height: 1.45;
  }
  .ln {
    /* Preserve spacing within a log line and wrap long ones, without <pre>'s whitespace pitfalls. */
    white-space: pre-wrap;
    word-break: break-word;
  }
  .ts {
    color: var(--text-muted);
    user-select: none;
    margin-right: 0.5rem;
  }
  .hint {
    margin: 0.25rem 0;
  }
  .hint.err {
    color: var(--fg-fail);
  }
  .live {
    color: var(--accent);
    font-size: 0.7rem;
  }
  .linkish {
    background: none;
    border: none;
    padding: 0;
    color: var(--accent);
    cursor: pointer;
    font-size: 0.72rem;
  }
  .linkish:disabled {
    color: var(--text-muted);
    cursor: default;
  }
</style>
