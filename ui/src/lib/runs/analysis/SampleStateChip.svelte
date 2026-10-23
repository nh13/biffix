<script lang="ts">
  // One sample's (or one step's) state as a small chip, in the samples grid's words and colours:
  // done, running, waiting, reused (a cell's work kept from an earlier analysis), failed, stopped,
  // and `none` (not started). A missing state is a dash, read out as `missing`. The one copy of the
  // chip's look, for the analysis page's samples grid and the experiment page's samples table.
  import type { Snippet } from 'svelte';

  interface Props {
    /** The state; null for none (a dash). */
    value: string | null;
    /** The words shown, when not the state itself (the legend's, "not started"). */
    label?: string | null;
    /** What a screen reader hears for the dash. */
    missing?: string;
    /** After the words, inside the chip (a cell's retry count). */
    children?: Snippet;
  }
  let { value, label = null, missing = 'not in this analysis', children }: Props = $props();
  const KNOWN = new Set(['done', 'running', 'waiting', 'reused', 'failed', 'stopped', 'none']);
  const tone = $derived(value && KNOWN.has(value) ? value : 'waiting');
</script>

{#if value}
  <span class="chip {tone}">{label ?? value}{@render children?.()}</span>
{:else}
  <span class="chip none"
    ><span aria-hidden="true">–</span><span class="sr-only">{missing}</span></span
  >
{/if}

<style>
  /* Larger than the shared chip: a sample's state is the grid's content, not a tag. */
  .chip {
    --fg-cached: #0e7490;
    font-size: 0.74rem;
    border: 1px solid transparent;
  }
  :global(html.dark) .chip {
    --fg-cached: #67e8f9;
  }
  /* Coloured text and outline on the page background: text colours meet AA there. */
  .done {
    color: var(--fg-ok);
    border-color: color-mix(in oklch, var(--fg-ok) 45%, transparent);
  }
  .running {
    color: var(--accent);
    border-color: color-mix(in oklch, var(--accent) 45%, transparent);
  }
  .waiting,
  .stopped,
  .none {
    color: var(--text-muted);
    border-color: var(--card-border);
  }
  .reused {
    color: var(--fg-cached);
    border-color: color-mix(in oklch, var(--fg-cached) 45%, transparent);
  }
  .failed {
    color: var(--fg-fail);
    border-color: color-mix(in oklch, var(--fg-fail) 45%, transparent);
  }
</style>
