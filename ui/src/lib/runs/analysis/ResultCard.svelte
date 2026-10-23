<script lang="ts">
  // One key result on the Lab analysis page: its name, format and description, a preview (a table's
  // first rows, an image as itself), Open (the output viewer, the FastQC viewer or the genome
  // browser, by its format) and Download. A per-sample result picks its sample first.
  import { getOutputPreview, getOutputViewUrl } from '$lib/api';
  import { headerlessFormatOf, parseTable } from '$lib/util/tsv';
  import { unservedText, type OpenTarget } from '$lib/qc/open';
  import ReusedFrom from '$lib/runs/ReusedFrom.svelte';
  import { DataLinks } from '$lib/data/dataLinks.svelte';
  import {
    formatLabel,
    previewKind,
    sampleCount,
    samplesText,
    type ResultCard,
  } from '$lib/runs/results';
  import { ResultChoice } from './resultChoice.svelte';

  interface Props {
    runId: string;
    /** The run's project, whose data sources the Data page shows the result in. */
    project?: string | null;
    card: ResultCard;
    /** The card's heading, when not the result's name (an analysis's, comparing one result). */
    heading?: string | null;
    /** Open a result (in the viewer its format calls for). */
    onopen: (target: OpenTarget) => void;
  }
  let { runId, project = null, card, heading = null, onopen }: Props = $props();

  const uid = $props.id();
  const choice = new ResultChoice(
    () => runId,
    () => card,
  );
  const entry = $derived(choice.entry);
  const output = $derived(choice.output);
  const format = $derived(choice.format);
  const file = $derived(choice.file);

  // Where the Data page shows it (its folder, with it open), when it can.
  const dataLinks = new DataLinks();
  $effect(() => {
    dataLinks.load(project);
  });
  const dataHref = $derived(
    output && output.exists !== false ? dataLinks.href(output.s3_uri) : null,
  );

  // The preview, keyed on the output id (a poll hands in new objects for the same output).
  const outputId = $derived(output?.id ?? null);
  const outputPath = $derived(output?.path ?? '');
  const kind = $derived(previewKind(format));
  let table = $state<{ header: string[]; rows: string[][] } | null>(null);
  let imageUrl = $state<string | null>(null);
  $effect(() => {
    const id = outputId;
    const path = outputPath;
    const want = kind;
    table = null;
    imageUrl = null;
    if (!id || !want) return;
    let cancelled = false;
    const load =
      want === 'table'
        ? getOutputPreview(runId, id).then((p) => {
            if (cancelled || p.kind !== 'text') return;
            const section = parseTable(p.text, p.truncated, headerlessFormatOf(path))?.sections[0];
            if (section)
              table = {
                header: section.header.slice(0, 4),
                rows: section.rows.slice(0, 3).map((r) => r.slice(0, 4)),
              };
          })
        : getOutputViewUrl(runId, id).then((u) => {
            if (!cancelled) imageUrl = u.url;
          });
    load.catch(() => {
      /* no preview: the card still opens and downloads */
    });
    return () => {
      cancelled = true;
    };
  });
</script>

<article class="card" aria-labelledby="{uid}-name">
  <div class="preview" class:image-tile={!table && !!imageUrl} aria-hidden={!table && !imageUrl}>
    {#if table}
      <table>
        <caption class="sr-only">First rows of {card.name}</caption>
        <thead
          ><tr
            >{#each table.header as h}<th scope="col">{h}</th>{/each}</tr
          ></thead
        >
        <tbody>
          {#each table.rows as row}<tr
              >{#each row as cell}<td>{cell}</td>{/each}</tr
            >{/each}
        </tbody>
      </table>
    {:else if imageUrl}
      <img src={imageUrl} alt={`Preview of ${card.name}`} />
    {:else if card.perSample}
      <span class="format-big"
        >{formatLabel(format)}{entry?.sample ? ` · ${entry.sample}` : ''}<span class="count"
          >{samplesText(sampleCount(card))}</span
        ></span
      >
    {:else}
      <span class="format-big">{formatLabel(format)}</span>
    {/if}
  </div>
  <div class="body">
    <h3 id="{uid}-name">{heading ?? card.name}</h3>
    <p class="meta">
      <span class="format">{formatLabel(format)}</span
      >{#if card.perSample}{' · one per sample'}{/if}{#if card.description}{' · '}{card.description}{/if}
    </p>
    {#if output?.reused_from}<p class="meta"><ReusedFrom from={output.reused_from} /></p>{/if}
    {#if card.perSample}
      <label class="sample">
        <span>Sample</span>
        <select
          value={entry?.report.output.id ?? ''}
          onchange={(e) => (choice.chosen = (e.currentTarget as HTMLSelectElement).value)}
        >
          {#each card.entries as e, i (e.report.output.id)}<option value={e.report.output.id}
              >{choice.labels[i]}</option
            >{/each}
        </select>
      </label>
    {/if}
    <div class="actions">
      {#if choice.target}
        {@const target = choice.target}
        <button class="btn btn-compact btn-primary" onclick={() => onopen(target)}
          >Open<span class="sr-only"> {file?.name}</span></button
        >
      {/if}
      {#if output && !choice.served}<span class="meta unserved">{unservedText(output)}</span>{/if}
      {#if file && choice.served}
        <button class="btn btn-compact" onclick={() => file.download()}
          >Download<span class="sr-only"> {file.name}</span></button
        >
      {/if}
      {#if dataHref}
        <a class="btn btn-compact" href={dataHref} data-testid="result-data-link"
          >Show in Data<span class="sr-only"> browser: {file?.name ?? card.name}</span></a
        >
      {/if}
    </div>
    {#if choice.downloadError}<p class="error" role="alert">{choice.downloadError}</p>{/if}
  </div>
</article>

<style>
  /* No padding: the preview runs to the card's edges, and the body below pads itself. */
  .card {
    padding: 0;
    display: flex;
    flex-direction: column;
    min-width: 0;
    overflow: hidden;
  }
  .preview {
    display: flex;
    align-items: center;
    justify-content: center;
    height: 7rem;
    padding: 0.4rem;
    background: var(--th-bg);
    border-bottom: 1px solid var(--card-border);
    overflow: hidden;
  }
  /* An image keeps its own (usually white) background, so it sits on a neutral checker tile in a
     light frame rather than a bright block inside a dark card. */
  .preview.image-tile {
    background-color: var(--tile-a);
    background-image: conic-gradient(
      var(--tile-b) 25%,
      transparent 0 50%,
      var(--tile-b) 0 75%,
      transparent 0
    );
    background-size: 14px 14px;
  }
  .preview img {
    max-width: 100%;
    max-height: 100%;
    object-fit: contain;
  }
  .preview.image-tile img {
    padding: 0.15rem;
    border-radius: 0.25rem;
    background: #fff;
    box-shadow: 0 0 0 1px var(--card-border);
  }
  .preview table {
    border-collapse: collapse;
    font-family: var(--mono);
    font-size: 0.66rem;
    background: var(--bg-content);
    max-width: 100%;
    table-layout: fixed;
  }
  .preview th,
  .preview td {
    padding: 0.1rem 0.35rem;
    border: 1px solid var(--card-border);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
    max-width: 5rem;
  }
  .format-big {
    display: flex;
    flex-direction: column;
    align-items: center;
    gap: 0.15rem;
    font-size: 0.9rem;
    color: var(--text-muted);
    text-align: center;
  }
  .count {
    font-size: 0.76rem;
  }
  .body {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    padding: 0.6rem 0.75rem 0.75rem;
  }
  h3 {
    margin: 0;
    font-size: 0.92rem;
  }
  .meta {
    margin: 0;
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .format {
    font-weight: 600;
  }
  .sample {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  select {
    min-width: 0;
    padding: 0.2rem 0.35rem;
    border: 1px solid var(--control-border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
  }
  .actions {
    margin-top: 0.2rem;
  }
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .error {
    margin: 0;
    font-size: 0.78rem;
  }
</style>
