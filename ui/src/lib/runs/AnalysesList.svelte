<script lang="ts">
  // The runs list in the Lab view: a table of analyses by name, pipeline and plain status, or below
  // a phone's width My work's rows (a table would not fit). The page owns the list's state.
  import { MediaQuery } from 'svelte/reactivity';
  import SortHeader from '#lib/components/SortHeader.svelte';
  import RunRow from '#lib/home/RunRow.svelte';
  import { fmtDate } from '#lib/util/format.js';
  import { pipelineTitle } from './runName';
  import { runTabHref } from './runNav';
  import { runVersionHref } from './runVersion';
  import type { SortKey } from './runsSort';
  import type { RunsList } from './runsList.svelte';
  import StatusPill from './StatusPill.svelte';
  import { statusLine } from './status';
  import RunCostCell from './RunCostCell.svelte';
  import DismissCell from './DismissCell.svelte';

  let { list }: { list: RunsList } = $props();

  const narrow = new MediaQuery('max-width: 640px');
  const SORTED_COLUMNS: [label: string, key: SortKey][] = [
    ['Analysis', 'name'],
    ['Pipeline', 'workflow'],
    ['Status', 'state'],
    ['Started', 'submitted'],
  ];
</script>

{#if narrow.current}
  <ul class="rows" data-testid="analyses-rows">
    {#each list.sorted as r (r.run_id)}
      <RunRow run={r} onDismiss={list.ownOnly ? () => list.dismissOne(r) : undefined} />
    {/each}
  </ul>
{:else}
  <table>
    <thead>
      <tr>
        {#each SORTED_COLUMNS as [label, key] (key)}
          <SortHeader
            {label}
            {key}
            sortKey={list.sortKey}
            sortDir={list.sortDir}
            onsort={list.toggleSort}
          />
        {/each}
        {#if list.showWho}
          <SortHeader
            label="Started by"
            key="submitter"
            sortKey={list.sortKey}
            sortDir={list.sortDir}
            onsort={list.toggleSort}
          />
        {/if}
        <th scope="col" class="num">Cost</th>
        {#if list.ownOnly}<th scope="col"><span class="sr-only">Dismiss</span></th>{/if}
      </tr>
    </thead>
    <tbody>
      {#each list.sorted as r (r.run_id)}
        <tr>
          <td
            ><a href={runTabHref(r.run_id, 'overview')}>{list.nameOf(r)}</a>{#if r.resumed}<span
                class="resumed"
                title="Continues an earlier analysis"
                ><span aria-hidden="true"> ↻</span><span class="sr-only">
                  (continues an earlier analysis)</span
                ></span
              >{/if}</td
          >
          <td>
            {#if r.workflow}<a
                href={runVersionHref(r)}
                title={`The pipeline version this analysis ran (${r.workflow})`}
                data-testid="pipeline-link">{pipelineTitle(r) ?? 'Pipeline'}</a
              >{:else}{pipelineTitle(r) ?? '--'}{/if}
          </td>
          <td><StatusPill compact line={statusLine(r)} /></td>
          <td>{fmtDate(r.submitted_at)}</td>
          {#if list.showWho}<td>{r.submitter_label ?? '--'}</td>{/if}
          <RunCostCell run={r} />
          {#if list.ownOnly}<DismissCell
              name={list.nameOf(r)}
              ondismiss={() => list.dismissOne(r)}
            />{/if}
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  .rows {
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    margin: 0 0 0.75rem;
    padding: 0;
  }
  .resumed {
    color: var(--text-muted);
  }
  /* A cell's link is its own line, not a link inside running text. */
  td > a {
    display: inline-block;
  }
  .num {
    text-align: right;
    white-space: nowrap;
  }
</style>
