<script lang="ts">
  // The samplesheet builder's editable table: a status line (every row ready, or which rows have
  // problems, with *Only rows with problems*), then a row per sample with its status, a cell per
  // column and its problems in words beneath it. A sample to point out on arrival is scrolled to,
  // focused and briefly highlighted.
  import { untrack } from 'svelte';
  import type { SheetSpec } from '#lib/generated/samples.js';
  import {
    columnLabel,
    firstProblemColumn,
    problemRowsSummary,
    problemText,
    rowName,
    sheetCellId,
  } from './sheet';
  import type { SheetState } from './sheetState.svelte';

  interface Props {
    sheet: SheetState;
    /** The param, for ids and labels. */
    name: string;
    label: string;
    spec: SheetSpec;
    /** A sample to point out on arrival (a failed analysis's *Fix and try again*). */
    highlight?: string | null;
  }
  let { sheet, name, label, spec, highlight = null }: Props = $props();

  // The highlighted sample's row: pointed out once, when it first shows, then the highlight fades.
  let flashing = $state<number | null>(null);
  let pointedOut = false;
  let sheetTable = $state<HTMLTableElement | null>(null);
  $effect(() => {
    const id = highlight;
    const col = sheet.idCol;
    const table = sheetTable;
    if (pointedOut || !id || !col || !table) return;
    // Trimmed, as every other id lookup (and the written sheet) reads it.
    const i = untrack(() => sheet.rows).findIndex((r) => (r[col.name] ?? '').trim() === id);
    if (i < 0) return;
    pointedOut = true;
    flashing = i;
    const input = table.querySelector<HTMLElement>(
      `[data-row="${i}"] input, [data-row="${i}"] select`,
    );
    input?.scrollIntoView({ block: 'center' });
    input?.focus({ preventScroll: true });
    setTimeout(() => (flashing = null), 4000);
  });

  function showFirstProblem() {
    const i = sheet.badRows[0];
    if (i === undefined) return;
    const column = firstProblemColumn(spec, sheet.rowProblems(i));
    const el = column ? document.getElementById(sheetCellId(name, i, column)) : null;
    el?.scrollIntoView({ block: 'center' });
    el?.focus({ preventScroll: true });
  }
</script>

{#if sheet.rows.length === 0}
  <p class="empty">
    No samples yet. Choose the sequencing files from a data source, or import a samplesheet.
  </p>
{:else}
  <div class="sheet-status" aria-live="polite">
    {#if sheet.badRows.length === 0}
      <span class="ok-text">✓ Every row is ready</span>
    {:else}
      <span class="warn-text">⚠ {problemRowsSummary(sheet.badRows)}</span>
      <button class="link" onclick={showFirstProblem}>Show the first →</button>
      <label class="only-problems">
        <input
          type="checkbox"
          checked={sheet.onlyProblems}
          onchange={(e) => sheet.setOnlyProblems(e.currentTarget.checked)}
        />
        Only rows with problems ({sheet.badRows.length})
      </label>
    {/if}
  </div>
  <div class="scroll">
    <table class="sheet" aria-label={`${label} rows`} bind:this={sheetTable}>
      <thead>
        <tr>
          <th class="flag">Status</th>
          {#each spec.columns as c (c.name)}
            <th title={c.description ?? undefined}>{columnLabel(c)}</th>
          {/each}
          <th><span class="sr-only">Remove</span></th>
        </tr>
      </thead>
      <tbody>
        {#each sheet.shownRows as i (i)}
          {@const row = sheet.rows[i]}
          {@const rp = sheet.rowProblems(i)}
          {@const said = rp.map((p) => problemText(spec, p))}
          {@const st = sheet.statuses[i]?.state ?? 'empty'}
          <tr data-row={i} class:flagged={st === 'problems'} class:highlighted={flashing === i}>
            <td class="flag">
              {#if st === 'ok'}
                <span class="ok-text" title="Ready"
                  >✓<span class="sr-only">Row {i + 1} ready</span></span
                >
              {:else if st === 'problems'}
                <span class="warn-text" title={said.join('\n')}
                  >⚠ {rp.length}<span class="sr-only">
                    {rp.length === 1 ? 'problem' : 'problems'} in row {i + 1}</span
                  ></span
                >
              {:else}
                <span class="muted" title="Empty: left out of the samplesheet"
                  >–<span class="sr-only">Row {i + 1} empty, left out</span></span
                >
              {/if}
            </td>
            {#each spec.columns as c (c.name)}
              {@const problem = sheet.cellProblem(i, c.name)}
              <td class:bad={!!problem}>
                {#if c.enum_values}
                  <select
                    id={sheetCellId(name, i, c.name)}
                    aria-label={`Row ${i + 1} ${c.name}`}
                    value={row[c.name] ?? ''}
                    onchange={(e) =>
                      sheet.setCell(i, c.name, (e.currentTarget as HTMLSelectElement).value)}
                  >
                    {#if !c.required}<option value="">—</option>{/if}
                    {#each c.enum_values as v}<option value={v}>{v}</option>{/each}
                  </select>
                {:else}
                  <input
                    id={sheetCellId(name, i, c.name)}
                    class:mono={c.file || c === sheet.idCol}
                    class:file={c.file}
                    spellcheck="false"
                    aria-label={`Row ${i + 1} ${c.name}`}
                    aria-invalid={!!problem}
                    title={problem ?? row[c.name] ?? ''}
                    value={row[c.name] ?? ''}
                    placeholder={c.file ? 's3://…' : ''}
                    oninput={(e) =>
                      sheet.setCell(i, c.name, (e.currentTarget as HTMLInputElement).value)}
                  />
                {/if}
              </td>
            {/each}
            <td>
              <button
                class="remove"
                aria-label={`Remove row ${i + 1}`}
                onclick={() => sheet.removeRow(i)}>×</button
              >
            </td>
          </tr>
          {#if st === 'problems'}
            <tr class="row-msg flagged">
              <td></td>
              <td colspan={spec.columns.length + 1}>
                <strong>{rowName(spec, sheet.rows, i)}:</strong>
                {said.join('; ')}
              </td>
            </tr>
          {/if}
        {/each}
      </tbody>
    </table>
  </div>
{/if}

<style>
  /* The table, cell and status-text rules the builder's three tables share are the builder's
     (SamplesheetBuilder.svelte); these are this table's own. */
  tr.highlighted td {
    background: var(--hover);
    box-shadow:
      inset 0 2px 0 var(--accent),
      inset 0 -2px 0 var(--accent);
  }
  tr.flagged td {
    background: color-mix(in oklch, var(--fg-warn) 10%, transparent);
  }
  td.bad input,
  td.bad select {
    border-color: var(--fg-fail);
  }
  .flag {
    width: 3.2rem;
    white-space: nowrap;
  }
  .ok-text {
    color: var(--fg-ok);
  }
  .warn-text {
    color: var(--fg-warn);
    font-weight: 600;
  }
  .sheet-status {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    margin-bottom: 0.4rem;
    font-size: 0.82rem;
  }
  .only-problems {
    display: inline-flex;
    align-items: center;
    gap: 0.3rem;
    margin-left: auto;
    color: var(--text-muted);
  }
  tr.row-msg td {
    padding-top: 0;
    font-size: 0.78rem;
    color: var(--text);
  }
  .sheet input,
  .sheet select {
    box-sizing: border-box;
    width: 100%;
    min-width: 7rem;
    font: inherit;
    font-size: 0.8rem;
    padding: 0.25rem 0.4rem;
    border: 1px solid var(--control-border);
    border-radius: 0.3rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .sheet input.file {
    min-width: 16rem;
    direction: rtl;
    text-align: left;
  }
  .sheet input.file:focus {
    direction: ltr;
  }
  .mono {
    font-size: 0.78rem;
  }
  .remove {
    border: none;
    background: none;
    color: var(--text-muted);
    font-size: 1rem;
    cursor: pointer;
  }
</style>
