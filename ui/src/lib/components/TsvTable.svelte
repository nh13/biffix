<script lang="ts">
  import { sortRows, type ParsedTable } from '$lib/util/tsv';
  import { plural } from '$lib/util/format';
  import { ariaSort, sortArrow, type SortDir } from '$lib/util/sort';

  // One parsed delimited-text output (metrics, TSV/CSV, BED, VCF) drawn as sortable, paged
  // tables: its leading comments, then each section with a sticky header.
  interface Props {
    table: ParsedTable;
    /** The text is only the first part of the file. */
    truncated: boolean;
  }
  let { table, truncated }: Props = $props();

  /** Rows drawn per page; more are added on request. */
  const PAGE = 200;

  type Sort = { col: number; descending: boolean } | null;

  // Per-section view state, reset when a different table is shown.
  let sorts = $state<Sort[]>([]);
  let shown = $state<number[]>([]);
  $effect(() => {
    sorts = table.sections.map(() => null);
    shown = table.sections.map(() => PAGE);
  });

  const sortedRows = $derived(
    table.sections.map((section, i) => {
      const sort = sorts[i];
      return sort
        ? sortRows(section.rows, sort.col, section.numeric[sort.col] ?? false, sort.descending)
        : section.rows;
    }),
  );

  /** Cycle a column's sort: ascending → descending → original order. */
  function cycleSort(sectionIndex: number, col: number) {
    const current = sorts[sectionIndex];
    let next: Sort;
    if (!current || current.col !== col) next = { col, descending: false };
    else if (!current.descending) next = { col, descending: true };
    else next = null;
    sorts[sectionIndex] = next;
  }

  // The shared indicators (`$lib/util/sort`), by column index; the three-state cycle is this
  // table's own.
  const sortKey = (si: number) => sorts[si]?.col ?? null;
  const sortDir = (si: number): SortDir => (sorts[si]?.descending ? 'desc' : 'asc');

  function showMore(sectionIndex: number) {
    shown[sectionIndex] = (shown[sectionIndex] ?? PAGE) + PAGE;
  }
</script>

<div class="tsv">
  {#if table.comments.length > 0}
    <pre class="comments">{table.comments.join('\n')}</pre>
  {/if}

  {#each table.sections as section, si (si)}
    {@const rows = sortedRows[si] ?? section.rows}
    {@const limit = shown[si] ?? PAGE}
    <div class="section">
      {#if section.title}<p class="section-title">{section.title}</p>{/if}
      <div class="scroll">
        <table>
          <thead>
            <tr>
              {#each section.header as name, ci (ci)}
                <th
                  scope="col"
                  class:num={section.numeric[ci]}
                  aria-sort={ariaSort(ci, sortKey(si), sortDir(si))}
                >
                  <button
                    type="button"
                    onclick={() => cycleSort(si, ci)}
                    title="Sort by {name || `column ${ci + 1}`}"
                  >
                    {name}
                    <span class="indicator" aria-hidden="true"
                      >{sortArrow(ci, sortKey(si), sortDir(si))}</span
                    >
                  </button>
                </th>
              {/each}
            </tr>
          </thead>
          <tbody>
            {#each rows.slice(0, limit) as row, ri (ri)}
              <tr>
                {#each row as cell, ci (ci)}
                  <td class:num={section.numeric[ci]}>{cell}</td>
                {/each}
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
      <div class="footer">
        <span class="muted"
          >{plural(section.rows.length, 'row')} × {plural(section.header.length, 'column')}</span
        >
        {#if rows.length > limit}
          <button type="button" class="more" onclick={() => showMore(si)}
            >Show more ({(rows.length - limit).toLocaleString()} remaining)</button
          >
        {/if}
      </div>
    </div>
  {/each}

  {#if truncated}
    <p class="notice muted">Showing the first 1 MiB of the file.</p>
  {/if}
</div>

<style>
  .section-title {
    margin: 0 0 0.3rem;
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--text-muted);
    white-space: pre-wrap;
  }
  .comments {
    margin: 0 0 0.5rem;
    padding: 0.4rem 0.6rem;
    max-height: 10rem;
    overflow: auto;
    font-family: var(--mono);
    font-size: 0.72rem;
    color: var(--text-muted);
    background: var(--th-bg);
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    white-space: pre;
  }
  .section + .section {
    margin-top: 1rem;
  }
  .scroll {
    overflow: auto;
    max-height: 60vh;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: var(--bg-content);
  }
  table {
    /* The global table style clips (overflow: hidden), which would break the sticky header. */
    width: auto;
    min-width: 100%;
    margin: 0;
    border: none;
    border-radius: 0;
    overflow: visible;
    border-collapse: separate;
    border-spacing: 0;
    font-size: 0.78rem;
  }
  th {
    position: sticky;
    top: 0;
    z-index: 1;
    padding: 0;
    background: var(--th-bg);
    text-transform: none;
    letter-spacing: normal;
    white-space: nowrap;
  }
  th button {
    display: flex;
    align-items: center;
    gap: 0.3rem;
    width: 100%;
    padding: 0.35rem 0.6rem;
    border: none;
    background: none;
    color: inherit;
    font: inherit;
    font-weight: 600;
    text-align: inherit;
    cursor: pointer;
  }
  th.num button {
    justify-content: flex-end;
  }
  th button:hover {
    color: var(--text);
  }
  .indicator {
    font-size: 0.65rem;
    min-width: 0.7rem;
  }
  td {
    padding: 0.25rem 0.6rem;
    font-family: var(--mono);
    white-space: nowrap;
    color: var(--text);
  }
  .num {
    text-align: right;
    font-variant-numeric: tabular-nums;
  }
  .footer {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.75rem;
    margin-top: 0.35rem;
    font-size: 0.75rem;
  }
  .more {
    padding: 0.15rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.35rem;
    background: none;
    color: inherit;
    font: inherit;
    cursor: pointer;
  }
  .more:hover {
    background: var(--hover);
  }
  .notice {
    margin: 0.5rem 0 0;
    font-size: 0.75rem;
  }
</style>
