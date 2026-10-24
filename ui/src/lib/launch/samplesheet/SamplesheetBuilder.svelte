<script lang="ts">
  // The samplesheet builder: a table of the workflow's own columns, filled
  // from files picked in the data sources (grouped into samples by the server's detection,
  // previewed before anything is added, with a naming editor for odd names), from an imported
  // CSV/TSV, or by hand. Rows are what the run submits; biffix writes the sheet file itself.
  //
  // This is the shell: the *Add samples* menu, export and Clear, and the three ways in. The rows'
  // state is `SheetState`, detection's `SampleDetection`, an import's `SheetImporter`; the table,
  // the detected-samples preview and the column-matching step are their own components.
  import type { SheetSpec } from '#lib/generated/samples.js';
  import { saveBlob } from '#lib/util/download.js';
  import { plural } from '#lib/util/format.js';
  import FilePicker from '../FilePicker.svelte';
  import DetectPreview from './DetectPreview.svelte';
  import ImportMapping from './ImportMapping.svelte';
  import SheetTable from './SheetTable.svelte';
  import { SampleDetection } from './detection.svelte';
  import { emptyRow, renderSheet, sheetFormat, type SheetRow } from './sheet';
  import { SheetImporter } from './sheetImporter.svelte';
  import { SheetState } from './sheetState.svelte';

  interface Props {
    /** The param, for ids and labels. */
    name: string;
    label: string;
    spec: SheetSpec;
    rows: SheetRow[];
    onchange: (rows: SheetRow[]) => void;
    /** Show every problem (after a launch attempt), not only those in rows already edited. */
    showErrors?: boolean;
    /** A sample to point out on arrival (a failed analysis's *Fix and try again*): its row is
     * scrolled to, focused and briefly highlighted. */
    highlight?: string | null;
    /** Where a CSV import's column mapping is remembered (the pipeline's param); none: not kept. */
    memoryKey?: string | null;
  }
  let {
    name,
    label,
    spec,
    rows,
    onchange,
    showErrors = false,
    highlight = null,
    memoryKey = null,
  }: Props = $props();

  const sheet = new SheetState({
    get spec() {
      return spec;
    },
    get rows() {
      return rows;
    },
    get showErrors() {
      return showErrors;
    },
    onchange: (next) => onchange(next),
  });
  const detection = new SampleDetection(() => spec);
  const importer = new SheetImporter(sheet, {
    get spec() {
      return spec;
    },
    get memoryKey() {
      return memoryKey;
    },
  });

  const format = $derived(sheetFormat(spec));
  const fileSuffixes = $derived([
    ...new Set(spec.columns.filter((c) => c.file).flatMap((c) => c.suffixes ?? [])),
  ]);

  let menuOpen = $state(false);
  let pickerOpen = $state(false);
  let importInput: HTMLInputElement;
  let menuWrap: HTMLElement;
  // Leaving the builder (a switch to an existing samplesheet file) drops a detection still pending.
  $effect(() => () => detection.close());
  /** Clear the table, once the user agrees: like an import, it drops every row. */
  function clearRows() {
    if (confirm(`Remove all ${plural(rows.length, 'row')} from the table?`)) sheet.replace([]);
  }

  function addEmptyRow() {
    menuOpen = false;
    sheet.addEmptyRow();
  }
  function addDetected() {
    if (!detection.preview) return;
    sheet.addDetected(detection.preview);
    detection.close();
  }
  async function importFile(e: Event) {
    const file = (e.currentTarget as HTMLInputElement).files?.[0];
    (e.currentTarget as HTMLInputElement).value = '';
    menuOpen = false;
    if (!file) return;
    await importer.read(file);
  }
  function exportCsv() {
    const text = renderSheet(spec, rows.length ? rows : [emptyRow(spec)]);
    const blob = new Blob([text], {
      type: format === 'tsv' ? 'text/tab-separated-values' : 'text/csv',
    });
    saveBlob(blob, `${name}.${format}`);
  }
</script>

<svelte:window
  onkeydown={(e) => {
    if (menuOpen && e.key === 'Escape') menuOpen = false;
  }}
  onpointerdown={(e) => {
    if (menuOpen && !menuWrap.contains(e.target as Node)) menuOpen = false;
  }}
/>

<div
  class="builder"
  id={`field-${name}`}
  tabindex="-1"
  role="group"
  aria-labelledby={`${name}-label`}
>
  <div class="toolbar">
    <div class="menu-wrap" bind:this={menuWrap}>
      <button
        class="btn btn-compact btn-primary"
        aria-haspopup="menu"
        aria-expanded={menuOpen}
        onclick={() => (menuOpen = !menuOpen)}>Add samples ▾</button
      >
      {#if menuOpen}
        <ul class="menu" role="menu">
          <li role="none">
            <button
              role="menuitem"
              onclick={() => {
                menuOpen = false;
                pickerOpen = true;
              }}>Choose files…</button
            >
          </li>
          <li role="none">
            <button
              role="menuitem"
              onclick={() => {
                // Closed now: a cancelled file dialog sends no change to close it.
                menuOpen = false;
                importInput.click();
              }}>Import CSV…</button
            >
          </li>
          <li role="none">
            <button role="menuitem" onclick={addEmptyRow}>Add an empty row</button>
          </li>
        </ul>
      {/if}
      <input
        class="hidden"
        type="file"
        accept=".csv,.tsv,.txt"
        bind:this={importInput}
        onchange={importFile}
        aria-label="Samplesheet file to import"
      />
    </div>
    <button class="btn btn-compact" onclick={exportCsv}
      >{rows.length ? 'Export' : 'Download template'}
      {format.toUpperCase()}</button
    >
    {#if rows.length}
      <button class="btn btn-compact" onclick={clearRows}>Clear</button>
    {/if}
    <span class="count">{plural(rows.length, 'row')}</span>
  </div>

  {#if importer.note}
    <p class="note" role="status">
      {importer.note}
      {#if importer.last && !importer.matching}
        <button class="link" onclick={() => importer.rematch()}>Change how its columns match</button
        >
      {/if}
    </p>
  {/if}

  {#if importer.matching}
    <ImportMapping
      matching={importer.matching}
      {name}
      {spec}
      preview={importer.matchPreview}
      onmap={(column, value) => importer.setMapping(column, value)}
      oncancel={() => importer.cancel()}
      onconfirm={() => importer.confirm()}
    />
  {/if}

  {#if detection.files.length > 0}
    <DetectPreview {detection} {name} onadd={addDetected} />
  {/if}

  {#each sheet.liveSuggestions as s (s.ids.join('|'))}
    <div class="suggestion" role="status">
      <strong>{s.ids[0]}</strong> and <strong>{s.ids[1]}</strong> look like one pair; the names
      differ only by case or leading zeros.
      <button class="btn btn-small" onclick={() => sheet.applySuggestion(s)}
        >Merge as {s.into}</button
      >
      <button class="link" onclick={() => sheet.dismissSuggestion(s)}>Keep separate</button>
    </div>
  {/each}

  <SheetTable {sheet} {name} {label} {spec} {highlight} />
</div>

{#if pickerOpen}
  <FilePicker
    mode="files"
    {label}
    suffixes={fileSuffixes.length ? fileSuffixes : ['.fastq.gz', '.fq.gz']}
    onpick={(uris) => detection.pick(uris)}
    onclose={() => (pickerOpen = false)}
  />
{/if}

<style>
  .builder {
    margin-top: 0.5rem;
  }
  .builder:focus {
    outline: none;
  }
  .toolbar {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    margin-bottom: 0.5rem;
  }
  .count {
    margin-left: auto;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .menu-wrap {
    position: relative;
  }
  .menu {
    position: absolute;
    z-index: 5;
    top: calc(100% + 0.25rem);
    left: 0;
    min-width: 13rem;
    margin: 0;
    padding: 0.3rem;
    list-style: none;
    background: var(--bg-content);
    border: 1px solid var(--control-border);
    border-radius: 0.5rem;
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.12);
  }
  .menu button {
    display: block;
    width: 100%;
    padding: 0.4rem 0.6rem;
    border: none;
    border-radius: 0.35rem;
    background: none;
    color: var(--text);
    text-align: left;
    font: inherit;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .menu button:hover {
    background: var(--hover);
  }
  .hidden {
    display: none;
  }
  /* Shared by the builder's parts (SheetTable, DetectPreview, ImportMapping): the boxes, their
     tables and their small text. Only the builder's own components render inside `.builder`, so
     these reach nothing else. Each part keeps its own `.mono` size: here it would outrank the
     parts' own input rules. */
  .builder :global(.preview),
  .suggestion,
  .note {
    border: 1px solid var(--card-border);
    border-left: 3px solid var(--accent);
    border-radius: 0.45rem;
    padding: 0.6rem 0.8rem;
    margin-bottom: 0.6rem;
    background: var(--hover);
    font-size: 0.83rem;
  }
  .suggestion {
    border-left-color: var(--fg-warn);
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
  }
  .builder :global(.preview h3) {
    margin: 0 0 0.3rem;
    font-size: 0.9rem;
  }
  .builder :global(.summary) {
    margin: 0 0 0.4rem;
    color: var(--text-muted);
  }
  .builder :global(.scroll) {
    overflow-x: auto;
    max-height: 26rem;
    overflow-y: auto;
  }
  .builder :global(table) {
    border-collapse: collapse;
    width: 100%;
    font-size: 0.8rem;
  }
  .builder :global(th),
  .builder :global(td) {
    text-align: left;
    padding: 0.25rem 0.4rem;
    border-bottom: 1px solid var(--card-border);
    vertical-align: middle;
  }
  .builder :global(th) {
    /* Column names are identifiers the workflow reads: show them exactly. */
    text-transform: none;
    letter-spacing: normal;
    position: sticky;
    top: 0;
    background: var(--th-bg);
    font-weight: 600;
    white-space: nowrap;
  }
  .builder :global(.preview-actions) {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.6rem;
  }
  .builder :global(.spacer) {
    flex: 1;
  }
  .builder :global(button:focus-visible),
  .builder :global(input:focus-visible),
  .builder :global(select:focus-visible) {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .builder :global(.empty),
  .builder :global(.muted) {
    color: var(--text-muted);
    font-size: 0.85rem;
  }
</style>
