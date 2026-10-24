<script lang="ts">
  import { progressView, type ProgressFacts } from '#lib/components/progress.js';
  // Unified cross-engine progress: one *component* for both engines, without pretending their
  // semantics are the same.
  // Progress = observed outcomes over a plan (when one exists). Snakemake reports a plan total, so
  // it renders a determinate bar; Nextflow does not, so it renders an indeterminate (pulsing) track
  // — which MUST look different from a filled bar (a filled fraction with no real denominator is
  // the false-completion lie). Outcome chips are engine-agnostic and shown for both. The arithmetic
  // is `progressView` (progress.ts), derived from `run` so it follows every poll.
  interface Props {
    run: ProgressFacts;
    /** Slim single-purpose rendering for a table cell. */
    compact?: boolean;
  }
  let { run, compact = false }: Props = $props();

  const v = $derived(progressView(run));
</script>

{#snippet chips()}
  <span class="chips">
    <span class="chip ok">{v.succeeded} done</span>
    {#if v.cached > 0}<span
        class="chip cached"
        title="Satisfied from the engine cache — not run this time">{v.cached} cached</span
      >{/if}
    {#if v.running > 0}<span class="chip run">{v.running} running</span>{/if}
    {#if v.queued > 0}<span
        class="chip queued"
        title="Live Batch attempts queued, awaiting capacity — not yet running"
        >{v.queued} queued</span
      >{/if}
    {#if v.failed > 0}<span class="chip fail">{v.failed} failed</span>{/if}
    {#if v.cancelled > 0}<span class="chip pend" title="Ended by the run's cancel"
        >{v.cancelled} cancelled</span
      >{/if}
    {#if v.unclassified > 0}<span
        class="chip pend"
        title={v.terminal ? 'Observed tasks whose terminal outcome was never reported' : undefined}
        >{v.unclassified} {v.terminal ? 'unreported' : 'pending'}</span
      >{/if}
  </span>
{/snippet}

<div class="progress" class:compact>
  {#if v.determinate}
    <div class="bar"><div class="fill" style:width={`${v.pct}%`}></div></div>
    <div class="line">
      <span class="count">{v.done} / {v.total} · {v.pct}%</span>
      {#if v.observed > 0}{@render chips()}{/if}
    </div>
  {:else if v.noJobs}
    <span class="muted">no jobs scheduled</span>
  {:else if v.terminal}
    <!-- Terminal with no plan (e.g. a finished Nextflow run): the outcomes ARE the result; showing
         a filled fraction would invent a denominator. Render chips only. -->
    <div class="line">
      {#if v.observed > 0}{@render chips()}{:else}<span class="muted">—</span>{/if}
    </div>
  {:else}
    <!-- Active with no plan: a pulsing track (deliberately not a filled fraction) and live
         outcomes. -->
    <div class="bar indeterminate"><div class="pulse"></div></div>
    <div class="line">
      {#if v.observed > 0}
        <!-- The engine finds its tasks as it runs, so the known count is a floor, not the
             total. -->
        <span
          class="count"
          title="Tasks known so far; the workflow is still finding more, so the total is not final"
          >{v.succeeded} of {v.observed}+ tasks</span
        >
        {@render chips()}
      {:else}<span class="muted">waiting…</span>{/if}
    </div>
  {/if}
</div>

<style>
  .progress {
    display: flex;
    flex-direction: column;
    gap: 0.3rem;
    min-width: 9rem;
  }
  .bar {
    position: relative;
    height: 0.5rem;
    border-radius: 0.3rem;
    background: var(--border);
    overflow: hidden;
  }
  .fill {
    height: 100%;
    background: #30a46c;
    border-radius: inherit;
    transition: width 0.4s ease;
  }
  /* The indeterminate track must read as "in progress, extent unknown" — an animated stripe that
     is visibly not a filled fraction. */
  .indeterminate {
    background: repeating-linear-gradient(
      -45deg,
      var(--border),
      var(--border) 6px,
      var(--bg-content) 6px,
      var(--bg-content) 12px
    );
  }
  .pulse {
    position: absolute;
    top: 0;
    bottom: 0;
    width: 35%;
    background: linear-gradient(90deg, transparent, #4a7cf5aa, transparent);
    animation: slide 1.4s ease-in-out infinite;
  }
  @keyframes slide {
    0% {
      left: -35%;
    }
    100% {
      left: 100%;
    }
  }
  .line {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  .count {
    font-variant-numeric: tabular-nums;
  }
  .chips {
    display: inline-flex;
    gap: 0.3rem;
    flex-wrap: wrap;
  }
  /* Smaller and tighter than the shared chip: several sit in one dense progress line. */
  .chip {
    padding: 0.02rem 0.35rem;
    font-size: 0.68rem;
    line-height: 1.4;
  }
  .chip.ok {
    color: var(--fg-ok);
    border-color: #30a46c55;
  }
  .chip.run {
    color: var(--accent);
    border-color: #4a7cf555;
  }
  /* Queued reads as "waiting, not yet active" — amber, distinct from the blue running chip. */
  .chip.queued {
    color: var(--fg-warn);
    border-color: #f5a62355;
  }
  .chip.cached {
    color: var(--text-muted);
    border-color: #8e8e9355;
  }
  .chip.fail {
    color: var(--fg-fail);
    border-color: #e5484d55;
  }
  .chip.pend {
    color: var(--text-muted);
  }
  .muted {
    font-size: 0.75rem;
  }
  .compact {
    min-width: 8rem;
    gap: 0.25rem;
  }
  .compact .bar {
    height: 0.4rem;
  }
</style>
