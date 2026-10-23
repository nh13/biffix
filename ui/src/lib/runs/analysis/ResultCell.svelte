<script lang="ts">
  // One analysis's result in the experiment's results table: Open (in the viewer its format calls
  // for) and Download, and, for a per-sample result with several samples, which sample. The table's
  // compact form of `ResultCard`, opening and downloading the same way.
  import { unservedText, type OpenTarget } from '$lib/qc/open';
  import type { ResultCard } from '$lib/runs/results';
  import { ResultChoice } from './resultChoice.svelte';

  interface Props {
    runId: string;
    card: ResultCard;
    /** Its analysis's name, so each control says whose result it opens. */
    analysis: string;
    onopen: (target: OpenTarget) => void;
  }
  let { runId, card, analysis, onopen }: Props = $props();

  const choice = new ResultChoice(
    () => runId,
    () => card,
  );
  const what = $derived(`${choice.file?.name ?? card.name} of ${analysis}`);
</script>

<div class="cell">
  {#if card.entries.length > 1}
    <select
      aria-label={`Sample for ${card.name} of ${analysis}`}
      value={choice.entry?.report.output.id ?? ''}
      onchange={(e) => (choice.chosen = (e.currentTarget as HTMLSelectElement).value)}
    >
      {#each card.entries as e, i (e.report.output.id)}<option value={e.report.output.id}
          >{choice.labels[i]}</option
        >{/each}
    </select>
  {:else if choice.entry?.sample}
    <span class="sample">{choice.entry.sample}</span>
  {/if}
  <span class="buttons">
    {#if choice.target}
      {@const target = choice.target}
      <button class="btn btn-small btn-primary" onclick={() => onopen(target)}
        >Open<span class="sr-only"> {what}</span></button
      >
    {/if}
    {#if choice.output && !choice.served}<span class="muted">{unservedText(choice.output)}</span
      >{/if}
    {#if choice.file && choice.served}
      <button class="btn btn-small" onclick={() => choice.file?.download()}
        >Download<span class="sr-only"> {what}</span></button
      >
    {/if}
  </span>
  {#if choice.downloadError}<span class="error" role="alert">{choice.downloadError}</span>{/if}
</div>

<style>
  .cell {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.3rem;
  }
  .buttons {
    display: inline-flex;
    gap: 0.3rem;
  }
  select {
    max-width: 9rem;
    font: inherit;
    font-size: 0.75rem;
    padding: 0.1rem 0.2rem;
    border: 1px solid var(--control-border);
    border-radius: 0.3rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .sample {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .error {
    font-size: 0.75rem;
  }
</style>
