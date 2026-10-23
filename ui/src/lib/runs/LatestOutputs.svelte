<script lang="ts">
  import { listOutputs, type RunOutput, type RunDetail } from '#lib/api/index.js';
  import { fmtBytes } from '#lib/util/format.js';
  import { startPoll } from '#lib/util/poll.js';
  import { runTabHref } from '#lib/runs/runNav.js';

  // A glance at the run's outputs, linking to the Outputs view for the rest.
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  const SHOWN = 6;
  const SIZING_MS = 2_500;
  const MAX_SIZING_ASKS = 40;
  let outputs = $state<RunOutput[]>([]);
  // The list's own count: a resumed run's includes the files it reused, which its projection's
  // `outputs_total` (what it registered itself) does not.
  let total = $state<number | null>(null);
  let loaded = $state(false);
  let error = $state<string | null>(null);

  // Primitives read off the run (the layout replaces `run` on every poll): refetched only when the
  // count changes, not on every poll, and asked again quickly while the server is still checking
  // the files' sizes in S3 (each ask checks a bounded number).
  const runId = $derived(run.run_id);
  const outputsTotal = $derived(run.outputs_total);
  $effect(() => {
    const id = runId;
    void outputsTotal;
    let asks = 0;
    return startPoll({
      fetch: () => {
        asks += 1;
        return listOutputs(id, { limit: SHOWN });
      },
      onData: (page) => {
        outputs = page.outputs;
        total = page.total;
        error = null;
      },
      onError: (m) => (error = m),
      onSettled: () => (loaded = true),
      until: (page) => (page.enrichment_pending ?? 0) === 0 || asks >= MAX_SIZING_ASKS,
      intervalMs: SIZING_MS,
    });
  });

  const href = $derived(runTabHref(runId, 'outputs'));
</script>

<section class="latest">
  <h2>Outputs</h2>
  {#if !loaded}
    <p class="muted">Loading…</p>
  {:else if error && outputs.length === 0}
    <p class="error">Couldn't load outputs: {error}</p>
  {:else if outputs.length === 0}
    <p class="muted">No outputs registered yet; they appear as tasks finish.</p>
  {:else}
    <ul>
      {#each outputs as o (o.id)}
        <li>
          <span class="mono path">{o.path}</span>
          <span class="muted size">{o.size != null ? fmtBytes(o.size) : ''}</span>
        </li>
      {/each}
    </ul>
    <p class="more"><a {href}>All {total ?? run.outputs_total ?? outputs.length} outputs →</a></p>
  {/if}
</section>

<style>
  .latest {
    margin-bottom: 1.5rem;
  }
  ul {
    margin: 0;
    padding: 0.4rem 0.9rem;
    list-style: none;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  li {
    display: flex;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.2rem 0;
    font-size: 0.8rem;
  }
  .path {
    font-family: var(--mono);
    font-size: 0.78rem;
    word-break: break-all;
  }
  .size {
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
  .more {
    font-size: 0.8rem;
    margin: 0.4rem 0 0;
  }
</style>
