<script lang="ts">
  import { getQcGeneralStats, type QcGeneralStats, type RunReport } from '#lib/api/index.js';
  import { runTabHref } from '#lib/runs/runNav.js';
  import { withViewParams } from '#lib/util/urlParam.js';
  import { resultCards } from '#lib/runs/results.js';
  import { qcStatus } from './qcStats';
  import { plural } from '#lib/util/format.js';

  // On a run's Overview: a pointer to its QC report (MultiQC) — with how many samples it covers and
  // which fail MultiQC's own rules — or to its other reports, when it has any.
  interface Props {
    runId: string;
    reports: RunReport[] | null;
  }
  let { runId, reports }: Props = $props();

  const multiqc = $derived(reports?.find((r) => r.kind === 'multiqc') ?? null);
  // How many reports, a per-sample one (each sample's files) counting once.
  const count = $derived(reports ? resultCards(reports).length : 0);
  // The chosen report is a file id: it rides in the fragment, as the Reports tab keeps it.
  const href = (r: RunReport) =>
    withViewParams(runTabHref(runId, 'reports'), new URLSearchParams({ report: r.output.id }));

  let stats = $state<QcGeneralStats | null>(null);
  const multiqcId = $derived(multiqc?.output.id ?? null);
  $effect(() => {
    const id = runId;
    stats = null;
    if (!multiqcId) return;
    let cancelled = false;
    getQcGeneralStats(id)
      .then((s) => {
        if (!cancelled) stats = s;
      })
      .catch(() => {
        /* no General Statistics */
      });
    return () => {
      cancelled = true;
    };
  });
  // Failing on a column the General Statistics table shows by default: a column MultiQC hides is
  // left out, so every sample named here has a failed cell in the table the link opens.
  const failing = $derived.by(() => {
    if (!stats) return [];
    const shown = stats.columns.filter((c) => !c.hidden);
    return stats.samples
      .filter((s) =>
        shown.some((c) => qcStatus(s.values[c.id], c.cond_formatting_rules) === 'fail'),
      )
      .map((s) => s.name);
  });
</script>

{#if reports && reports.length > 0}
  <p class="reports-line">
    {#if multiqc}
      <a href={href(multiqc)}>QC report available →</a>
      {#if stats && stats.samples.length > 0}
        <span class="muted">· {plural(stats.samples.length, 'sample')}</span>
        {#if failing.length > 0}
          <span class="failing"
            >· {failing.length} failing QC: {failing.slice(0, 5).join(', ')}{failing.length > 5
              ? ', …'
              : ''}</span
          >
        {/if}
      {/if}
      {#if count > 1}<span class="muted">· {count - 1} more</span>{/if}
    {:else}
      <a href={href(reports[0])}>{count === 1 ? reports[0].name : `${count} reports`} →</a>
    {/if}
  </p>
{/if}

<style>
  .reports-line {
    margin: 0 0 1rem;
    font-size: 0.9rem;
  }
  .reports-line a {
    color: var(--accent);
    font-weight: 600;
  }
  .failing {
    color: var(--fg-fail);
    font-weight: 600;
  }
</style>
