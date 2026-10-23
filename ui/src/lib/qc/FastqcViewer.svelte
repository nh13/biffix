<script lang="ts">
  // The FastQC viewer: the pipeline's own FastQC result for one output, as data —
  // its modules with pass / warn / fail, the basic statistics, and each module's plot or table —
  // with the full FastQC HTML report one click away. A run with no FastQC data for the file (404),
  // or whose zip cannot be read (422), shows its HTML report instead, in the output viewer.
  import { untrack } from 'svelte';
  import {
    ApiError,
    getOutput,
    getOutputFastqc,
    type FastqcModule,
    type FastqcReport,
    type RunOutput,
  } from '$lib/api';
  import Dialog from '$lib/components/Dialog.svelte';
  import { errorKind } from '$lib/components/errorPage';
  import { errorMessage } from '$lib/util/format';
  import OutputViewer from '$lib/components/OutputViewer.svelte';
  import { outputViewerFile, type ViewerFile } from '$lib/components/viewerFile';
  import FastqcPlot from './FastqcPlot.svelte';
  import { extrasOf, statusOf, statusSummary, viewOf } from './fastqc';

  interface Props {
    runId: string;
    /** The output opened: a `_fastqc.zip`, a `_fastqc.html` or a FASTQ. */
    output: RunOutput;
    /** What the viewer is titled (the result's name, and its sample). */
    title: string;
    onclose: () => void;
  }
  let { runId, output, title, onclose }: Props = $props();

  const uid = $props.id();
  // What opened the viewer, where focus returns on close: the full report swaps this dialog for the
  // output viewer's and back, and each dialog alone would only know the one before it.
  const opener =
    typeof document !== 'undefined' && document.activeElement instanceof HTMLElement
      ? document.activeElement
      : null;
  let report = $state<FastqcReport | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  // FastQC's own HTML report: the output itself when it is one, else the zip's sibling.
  let htmlFile = $state<ViewerFile | null>(null);
  let showHtml = $state(false);
  let downloadError = $state<string | null>(null);
  let selected = $state(0);

  const fileOf = (o: RunOutput) =>
    outputViewerFile(runId, o, {
      name: title,
      detail: 'FastQC report',
      onDownloadError: (m) => (downloadError = m),
    });

  // Keyed on the ids, not the objects: a poll hands in new objects for the same output.
  const outputId = $derived(output.id);
  $effect(() => {
    const id = outputId;
    const opened = untrack(() => output);
    let cancelled = false;
    loading = true;
    error = null;
    report = null;
    htmlFile = opened.viewer === 'html' ? fileOf(opened) : null;
    showHtml = false;
    selected = 0;
    getOutputFastqc(runId, id)
      .then(async (r) => {
        if (cancelled) return;
        if (!r) {
          // No FastQC data: the HTML report as it is, when this is one.
          showHtml = htmlFile !== null;
          if (!htmlFile)
            error = 'No FastQC data was found in this file. Download it to look inside.';
          return;
        }
        report = r;
        const first = r.modules.findIndex((m) => statusOf(m.status).tone !== 'pass');
        selected = Math.max(0, first);
        if (!htmlFile && r.html_output_id && r.html_output_id !== id) {
          // Its HTML report, by id: one no longer listed is left out; any other failure is said.
          const html = await getOutput(runId, r.html_output_id).catch((e) => {
            if (!cancelled && errorKind(e) !== 'not-found')
              downloadError = `Couldn't find the full FastQC report: ${errorMessage(e)}`;
            return null;
          });
          if (!cancelled && html) htmlFile = fileOf(html);
        }
      })
      .catch((e) => {
        if (cancelled) return;
        // A zip that cannot be read (422): the HTML report, when this is one.
        if (e instanceof ApiError && e.status === 422 && htmlFile) showHtml = true;
        else error = errorMessage(e);
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  const modules = $derived(report?.modules ?? []);
  const current = $derived<FastqcModule | null>(modules[selected] ?? null);
  const summary = $derived(statusSummary(modules));
  let asTable = $state(false);
  $effect(() => {
    void selected;
    asTable = false;
  });

  function closeHtml() {
    // Back to the modules when there are any; a fallback report closes the viewer.
    if (report) showHtml = false;
    else onclose();
  }
</script>

{#if showHtml && htmlFile}
  <OutputViewer file={htmlFile} onclose={closeHtml} returnFocus={opener} />
{:else}
  <Dialog labelledby="{uid}-title" {onclose} returnFocus={opener}>
    <header>
      <div class="title">
        <h3 id="{uid}-title">{title}</h3>
        <p class="muted meta">
          {#if report}From the pipeline's FastQC{report.version
              ? ` (v${report.version})`
              : ''}{#if summary}{' · '}{summary}{/if}{:else}{output.name}{/if}
        </p>
      </div>
      <div class="actions">
        {#if htmlFile}
          <button type="button" class="link" onclick={() => (showHtml = true)}
            >Full FastQC report</button
          >
        {/if}
        <button type="button" class="close" onclick={onclose} aria-label="Close">✕</button>
      </div>
    </header>
    <div class="body">
      {#if loading}
        <p class="muted">Loading…</p>
      {:else if error}
        <p class="error" role="alert">Couldn't open the FastQC result: {error}</p>
      {:else if report && modules.length === 0}
        <p class="muted">This FastQC result has no modules.</p>
      {:else if report}
        <div class="layout">
          <nav aria-label="FastQC modules">
            <ul>
              {#each modules as m, i (i)}
                {@const st = statusOf(m.status)}
                <li>
                  <button
                    type="button"
                    class="module"
                    aria-current={i === selected ? 'true' : undefined}
                    onclick={() => (selected = i)}
                  >
                    <span class="badge {st.tone}" aria-hidden="true">{st.icon}</span>
                    <span class="name">{m.name}<span class="sr-only">: {st.label}</span></span>
                  </button>
                </li>
              {/each}
            </ul>
          </nav>
          {#if current}
            {@const st = statusOf(current.status)}
            {@const view = viewOf(current)}
            {@const extras = extrasOf(current)}
            <section class="module-body" aria-labelledby="{uid}-module">
              <div class="module-head">
                <h4 id="{uid}-module">{current.name}</h4>
                <span class="status {st.tone}"
                  ><span aria-hidden="true">{st.icon}</span> {st.label}</span
                >
                {#if view !== 'table'}
                  <button type="button" class="link" onclick={() => (asTable = !asTable)}
                    >{asTable ? 'Show the plot' : 'Show the numbers'}</button
                  >
                {/if}
              </div>
              {#if extras.length}
                <dl class="extras">
                  {#each extras as [k, v]}<div>
                      <dt>{k}</dt>
                      <dd>{v}</dd>
                    </div>{/each}
                </dl>
              {/if}
              {#if view !== 'table' && !asTable}
                <FastqcPlot module={current} {view} />
              {:else if current.rows.length === 0}
                <p class="muted">Nothing to show: FastQC found nothing for this module.</p>
              {:else}
                <div class="table-wrap">
                  <table>
                    <caption class="sr-only">{current.name}</caption>
                    <thead>
                      <tr
                        >{#each current.columns as c}<th scope="col">{c}</th>{/each}</tr
                      >
                    </thead>
                    <tbody>
                      {#each current.rows as row}
                        <tr
                          >{#each row as cell}<td>{cell}</td>{/each}</tr
                        >
                      {/each}
                    </tbody>
                  </table>
                </div>
              {/if}
              {#if current.truncated}
                <p class="muted note">Showing the first {current.rows.length} rows.</p>
              {/if}
            </section>
          {/if}
        </div>
      {/if}
      {#if downloadError}<p class="error" role="alert">{downloadError}</p>{/if}
    </div>
  </Dialog>
{/if}

<style>
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
  }
  .title {
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: 0.95rem;
    overflow-wrap: anywhere;
  }
  .meta {
    margin: 0.2rem 0 0;
    font-size: 0.78rem;
  }
  /* Text links on the title's line: wider apart than buttons, and never wrapped under it. */
  .actions {
    flex-wrap: nowrap;
    gap: 0.75rem;
    white-space: nowrap;
  }
  .body {
    padding: 0.75rem 1rem 1rem;
    overflow: auto;
  }
  .layout {
    display: grid;
    grid-template-columns: minmax(12rem, 16rem) minmax(0, 1fr);
    gap: 1rem;
    align-items: start;
  }
  nav ul {
    list-style: none;
    margin: 0;
    padding: 0;
    display: grid;
    gap: 0.15rem;
  }
  .module {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    width: 100%;
    padding: 0.35rem 0.5rem;
    border: 1px solid transparent;
    border-radius: 0.4rem;
    background: none;
    color: var(--text);
    font: inherit;
    font-size: 0.82rem;
    text-align: left;
    cursor: pointer;
  }
  .module:hover {
    background: var(--hover);
  }
  .module[aria-current='true'] {
    background: var(--active);
    border-color: var(--card-border);
    font-weight: 600;
  }
  .badge {
    flex: none;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.1rem;
    height: 1.1rem;
    border-radius: 50%;
    border: 1.5px solid currentColor;
    font-size: 0.7rem;
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
  .other {
    color: var(--text-muted);
  }
  .module-body {
    min-width: 0;
  }
  .module-head {
    display: flex;
    flex-wrap: wrap;
    align-items: baseline;
    gap: 0.4rem 0.8rem;
    margin-bottom: 0.5rem;
  }
  h4 {
    margin: 0;
    font-size: 0.92rem;
  }
  .status {
    font-size: 0.8rem;
    font-weight: 600;
  }
  .extras {
    display: flex;
    flex-wrap: wrap;
    gap: 0.3rem 1.2rem;
    margin: 0 0 0.5rem;
    font-size: 0.8rem;
  }
  .extras dt {
    display: inline;
    color: var(--text-muted);
  }
  .extras dt::after {
    content: ': ';
  }
  .extras dd {
    display: inline;
    margin: 0;
  }
  .table-wrap {
    max-height: 60vh;
    overflow: auto;
  }
  table {
    margin-top: 0;
    font-size: 0.8rem;
  }
  td {
    font-family: var(--mono);
    font-size: 0.76rem;
  }
  .note {
    margin: 0.4rem 0 0;
    font-size: 0.76rem;
  }
  /* A dotted rule instead of an underline, as the viewer's other actions. */
  button.link {
    text-decoration: none;
    border-bottom: 1px dotted currentColor;
    font-size: 0.8rem;
  }
  .close {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: var(--text-muted);
    padding: 0 0.25rem;
  }
  .close:hover {
    color: inherit;
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  @media (max-width: 640px) {
    .layout {
      grid-template-columns: 1fr;
    }
  }
</style>
