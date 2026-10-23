<script lang="ts">
  // The column-matching step of a samplesheet import, shown when the file's columns are not named
  // like the sheet's: which of its columns fills each one, and a preview of the first rows that
  // match makes, before anything is imported.
  import type { SheetSpec } from '$lib/generated/samples';
  import { plural } from '$lib/util/format';
  import { columnLabel, columnNoun } from './sheet';
  import type { Matching } from './sheetImporter.svelte';
  import type { SheetRow } from './sheet';

  interface Props {
    matching: Matching;
    /** The param, for ids. */
    name: string;
    spec: SheetSpec;
    /** The first rows the match makes. */
    preview: SheetRow[];
    onmap: (column: string, value: string) => void;
    oncancel: () => void;
    onconfirm: () => void;
  }
  let { matching: m, name, spec, preview, onmap, oncancel, onconfirm }: Props = $props();
</script>

<section class="preview matching" aria-labelledby={`match-${name}`}>
  <h3 id={`match-${name}`}>Match columns</h3>
  <p class="summary">
    {m.file}'s columns are not named like this pipeline's. Choose which of its columns fills each
    one; the rest are left out.
  </p>
  <div class="match-rows">
    {#each spec.columns as c (c.name)}
      <label class="match-row">
        <span class="match-col"
          >{columnLabel(c)}{#if c.role}<span class="muted">
              · {columnNoun(spec, c.name)}</span
            >{/if}</span
        >
        <select
          value={m.mapping[c.name] === null ? '' : String(m.mapping[c.name])}
          onchange={(e) => onmap(c.name, (e.currentTarget as HTMLSelectElement).value)}
        >
          <option value="">{c.default ? `— leave as ${c.default} —` : '— leave empty —'}</option>
          {#each m.table.headers as h, i (i)}
            {#if h}<option value={String(i)}>{h}</option>{/if}
          {/each}
        </select>
      </label>
    {/each}
  </div>
  <h4 class="match-preview-title">Preview of the first {plural(preview.length, 'row')}</h4>
  <div class="scroll">
    <table aria-label="Import preview">
      <thead>
        <tr
          >{#each spec.columns as c (c.name)}<th>{c.name}</th>{/each}</tr
        >
      </thead>
      <tbody>
        {#each preview as r, i (i)}
          <tr>
            {#each spec.columns as c (c.name)}<td class:mono={c.file}>{r[c.name] || '—'}</td>{/each}
          </tr>
        {/each}
      </tbody>
    </table>
  </div>
  <div class="preview-actions">
    <span class="spacer"></span>
    <button class="btn btn-compact" onclick={oncancel}>Cancel</button>
    <button class="btn btn-compact btn-primary" onclick={onconfirm}
      >Import {plural(m.table.records.length, 'row')}</button
    >
  </div>
</section>

<style>
  /* The preview box, its table and its actions row are the builder's (SamplesheetBuilder.svelte),
     shared with the detected-samples preview; these are the matching step's own. */
  .matching .match-rows {
    display: flex;
    flex-direction: column;
    gap: 0.35rem;
    margin-bottom: 0.6rem;
  }
  .match-row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
  }
  .match-col {
    min-width: 11rem;
    font-weight: 600;
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .match-col .muted {
    font-family: inherit;
    font-weight: 400;
  }
  .match-row select {
    font: inherit;
    font-size: 0.82rem;
    padding: 0.25rem 0.45rem;
    border: 1px solid var(--control-border);
    border-radius: 0.35rem;
    background: var(--bg-content);
    color: var(--text);
    min-width: 12rem;
    max-width: 100%;
  }
  .mono {
    font-size: 0.78rem;
  }
  .match-preview-title {
    margin: 0.3rem 0;
    font-size: 0.82rem;
  }
</style>
