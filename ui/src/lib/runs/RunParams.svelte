<script lang="ts">
  import type { RunDetail } from '$lib/api';
  import { paramEntries } from '$lib/runs/runFacts';

  // The run's resolved params, secret-redacted server-side.
  interface Props {
    run: RunDetail;
  }
  let { run }: Props = $props();

  const entries = $derived(paramEntries(run.params));
</script>

<section class="params">
  <h2>Parameters</h2>
  {#if entries.length > 0}
    <dl class="param-list">
      {#each entries as [key, value]}
        <dt>{key}</dt>
        <dd>{typeof value === 'object' ? JSON.stringify(value) : String(value)}</dd>
      {/each}
    </dl>
  {:else}
    <p class="muted">This run has no parameters.</p>
  {/if}
</section>

<style>
  .params {
    margin-bottom: 1.25rem;
  }
  .param-list {
    display: grid;
    grid-template-columns: auto 1fr;
    gap: 0.2rem 0.9rem;
    margin: 0;
    padding: 0.6rem 0.9rem;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .param-list dt {
    font-size: 0.78rem;
    font-weight: 600;
    color: var(--text-muted);
  }
  .param-list dd {
    margin: 0;
    font-family: var(--mono);
    font-size: 0.78rem;
    color: var(--text);
    word-break: break-all;
  }
</style>
