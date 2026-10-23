<script lang="ts">
  // QC at a glance's FastQC for a run whose pipeline published MultiQC's output but not FastQC's
  // own zips: each result's module statuses and basic statistics as MultiQC
  // recorded them, with a sample picker (and a section picker when MultiQC ran FastQC more than
  // once: raw and trimmed reads), and the full MultiQC report (which has the plots) a click away.
  // Statuses only: plots need FastQC's zips, which this run did not publish.
  import type { FastqcStatusReport, RunReport } from '$lib/api';
  import { outputViewerFile } from '$lib/components/viewerFile';
  import { statusOf, statusSummary } from './fastqc';
  import { basicStatRows, samePick, statusSections } from './fastqcStatus';
  import type { OpenTarget } from './open';
  import ResultViewer from './ResultViewer.svelte';

  let {
    runId,
    report,
    multiqc,
  }: {
    runId: string;
    report: FastqcStatusReport;
    /** The run's MultiQC report (the one the statuses were read from), to open in the viewer. */
    multiqc: RunReport | null;
  } = $props();

  const uid = $props.id();
  const sections = $derived(statusSections(report.samples));
  let sectionPick = $state(0);
  const section = $derived(sections[sectionPick] ?? sections[0] ?? null);
  const choices = $derived(section?.choices ?? []);
  // Kept across a section change by what it is, not by its place (`samePick`).
  let pick = $state(0);
  /** `pick`, or the first choice when this section has fewer. */
  const at = $derived(choices[pick] ? pick : 0);
  function pickSection(next: number) {
    const to = sections[next]?.choices ?? [];
    pick = samePick(choices, at, to);
    sectionPick = next;
  }
  const chosen = $derived(choices[at] ?? null);
  // What is shown, said in full: the section too when there are several, so raw and trimmed
  // results of one FASTQ are never confused.
  const shown = $derived(
    chosen && section && sections.length > 1 ? `${chosen.label} · ${section.label}` : chosen?.label,
  );
  const summary = $derived(chosen ? statusSummary(chosen.result.modules) : '');
  const stats = $derived(chosen ? basicStatRows(chosen.result.stats) : []);
  let open = $state<OpenTarget | null>(null);
  let downloadError = $state<string | null>(null);

  function openMultiqc() {
    if (!multiqc) return;
    open = {
      kind: 'viewer',
      file: outputViewerFile(runId, multiqc.output, {
        name: multiqc.name,
        detail: multiqc.description,
        onDownloadError: (m) => (downloadError = m),
      }),
    };
  }
</script>

{#if chosen}
  <div class="fastqc-mqc" data-testid="fastqc-from-multiqc">
    <div class="row">
      <span class="label" id="{uid}-pick">FastQC per sample (from MultiQC)</span>
      {#if sections.length > 1}
        <select
          aria-label="FastQC section"
          value={String(sectionPick)}
          onchange={(e) => pickSection(Number((e.currentTarget as HTMLSelectElement).value))}
        >
          {#each sections as s, i (i)}<option value={String(i)}>{s.label}</option>{/each}
        </select>
      {/if}
      {#if choices.length > 1}
        <select
          aria-labelledby="{uid}-pick"
          value={String(at)}
          onchange={(e) => (pick = Number((e.currentTarget as HTMLSelectElement).value))}
        >
          {#each choices as c, i (i)}<option value={String(i)}>{c.label}</option>{/each}
        </select>
      {:else}
        <span class="only">{chosen.label}</span>
      {/if}
      {#if multiqc}
        <button class="btn btn-compact" onclick={openMultiqc}>Open the MultiQC report</button>
      {/if}
    </div>
    <p class="note">
      Status only: this pipeline published MultiQC's summary of FastQC, not FastQC's own results, so
      there are no plots here. The MultiQC report has them.
    </p>
    <div class="detail">
      <div>
        <p class="summary">
          <strong>{chosen.result.fastqc_name}</strong>
          {#if section && sections.length > 1}<span class="muted">· section {section.label}</span
            >{/if}
          {#if chosen.result.filename}<span class="muted">· {chosen.result.filename}</span>{/if}
          {#if summary}<span class="muted">· {summary}</span>{/if}
        </p>
        <ul class="modules" aria-label="FastQC modules · {shown}">
          {#each chosen.result.modules as m, i (i)}
            {@const st = statusOf(m.status)}
            <li>
              <span class="badge {st.tone}" aria-hidden="true">{st.icon}</span>
              <span class="name">{m.name}</span><span class="status {st.tone}">: {st.label}</span>
            </li>
          {/each}
        </ul>
      </div>
      {#if stats.length}
        <dl class="stats" aria-label="Basic statistics · {shown}">
          {#each stats as [label, value] (label)}
            <div>
              <dt>{label}</dt>
              <dd>{value}</dd>
            </div>
          {/each}
        </dl>
      {/if}
    </div>
    {#if downloadError}<p class="error" role="alert">{downloadError}</p>{/if}
  </div>
{/if}

{#if open}
  <ResultViewer target={open} onclose={() => (open = null)} />
{/if}

<style>
  .fastqc-mqc {
    margin-top: 0.75rem;
    font-size: 0.85rem;
  }
  .row {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem 0.75rem;
  }
  .label {
    min-width: 9rem;
    font-weight: 600;
  }
  .only {
    color: var(--text-muted);
  }
  .note {
    margin: 0.35rem 0 0.5rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .summary {
    margin: 0 0 0.35rem;
    overflow-wrap: anywhere;
  }
  .detail {
    display: flex;
    flex-wrap: wrap;
    gap: 0.75rem 2rem;
  }
  .modules {
    margin: 0;
    padding: 0;
    list-style: none;
    display: grid;
    gap: 0.15rem;
  }
  .badge {
    display: inline-block;
    width: 1.1rem;
    text-align: center;
    font-weight: 700;
  }
  .pass {
    color: var(--fg-ok);
  }
  .warn {
    color: var(--fg-warn);
  }
  .fail {
    color: var(--fg-fail);
  }
  .other,
  .status.pass {
    color: var(--text-muted);
  }
  .stats {
    margin: 0;
    display: grid;
    grid-template-columns: max-content;
    gap: 0.15rem;
    align-content: start;
  }
  .stats div {
    display: flex;
    gap: 0.5rem;
  }
  .stats dt {
    min-width: 9rem;
    color: var(--text-muted);
  }
  .stats dd {
    margin: 0;
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
  .error {
    margin: 0.4rem 0 0;
    font-size: 0.78rem;
  }
</style>
