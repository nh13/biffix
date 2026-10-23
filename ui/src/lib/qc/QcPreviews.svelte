<script lang="ts">
  // QC at a glance's previews: each sample's FastQC (the pipeline's own) and
  // its alignments in the genome browser, a sample picker and an Open each. Raw FASTQ inputs get
  // nothing here: only what the pipeline itself wrote.
  import type { RunReport } from '$lib/api';
  import { qcChoices, type OpenTarget, type QcChoice } from './open';
  import ResultViewer from './ResultViewer.svelte';

  let { runId, reports }: { runId: string; reports: readonly RunReport[] } = $props();

  const uid = $props.id();
  const choices = $derived(qcChoices(reports));
  let fastqcPick = $state<string | null>(null);
  let alignmentPick = $state<string | null>(null);
  let open = $state<OpenTarget | null>(null);

  /** The chosen entry by output id, else the first. */
  const chosen = (list: QcChoice[], id: string | null) =>
    list.find((c) => c.report.output.id === id) ?? list[0] ?? null;

  function openChoice(kind: 'fastqc' | 'alignment', c: QcChoice | null) {
    if (!c) return;
    open = { kind, runId, output: c.report.output, title: `${c.report.name} · ${c.label}` };
  }
</script>

{#snippet row(
  kind: 'fastqc' | 'alignment',
  heading: string,
  action: string,
  list: QcChoice[],
  pick: string | null,
  onpick: (id: string) => void,
)}
  {@const c = chosen(list, pick)}
  <div class="row">
    <span class="label" id="{uid}-{kind}">{heading}</span>
    {#if list.length > 1}
      <select
        aria-labelledby="{uid}-{kind}"
        value={c?.report.output.id ?? ''}
        onchange={(e) => onpick((e.currentTarget as HTMLSelectElement).value)}
      >
        {#each list as x (x.report.output.id)}<option value={x.report.output.id}>{x.label}</option
          >{/each}
      </select>
    {:else if c}
      <span class="only">{c.label}</span>
    {/if}
    <button class="btn btn-compact" onclick={() => openChoice(kind, c)}
      >{action}<span class="sr-only"> · {c?.label}</span></button
    >
  </div>
{/snippet}

<div class="previews">
  {#if choices.fastqc.length}
    {@render row(
      'fastqc',
      'FastQC per sample',
      'Open FastQC',
      choices.fastqc,
      fastqcPick,
      (id) => (fastqcPick = id),
    )}
  {/if}
  {#if choices.alignments.length}
    {@render row(
      'alignment',
      'Alignments',
      'View alignments',
      choices.alignments,
      alignmentPick,
      (id) => (alignmentPick = id),
    )}
  {/if}
</div>

{#if open}
  <ResultViewer target={open} onclose={() => (open = null)} />
{/if}

<style>
  .previews {
    display: grid;
    gap: 0.5rem;
    margin-top: 0.75rem;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem 0.75rem;
    font-size: 0.85rem;
  }
  .label {
    min-width: 9rem;
    font-weight: 600;
  }
  .only {
    color: var(--text-muted);
  }
  select {
    min-width: 0;
    padding: 0.25rem 0.4rem;
    border: 1px solid var(--control-border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
  }
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
