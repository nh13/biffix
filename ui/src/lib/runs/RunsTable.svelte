<script lang="ts">
  // The runs list in the Expert view: one row per run with its ids, workflow version, state,
  // engine, project, times, cost, submitter and progress. The page owns the list's state.
  import Progress from '#lib/components/Progress.svelte';
  import SortHeader from '#lib/components/SortHeader.svelte';
  import { fmtDate, fmtWallTime } from '#lib/util/format.js';
  import type { RunsList } from './runsList.svelte';
  import type { SortKey } from './runsSort';
  import { runTabHref } from './runNav';
  import { runVersionHref } from './runVersion';
  import StatusPill from './StatusPill.svelte';
  import RunCostCell from './RunCostCell.svelte';
  import DismissCell from './DismissCell.svelte';

  let { list }: { list: RunsList } = $props();

  const SORTED_COLUMNS: [label: string, key: SortKey][] = [
    ['Run', 'run'],
    ['Workflow', 'workflow'],
    ['Status', 'state'],
    ['Engine', 'engine'],
    ['Project', 'project'],
    ['Submitted', 'submitted'],
    ['Completed', 'completed'],
  ];
</script>

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
      <th scope="col">Wall time</th>
      <th scope="col" class="num">Cost</th>
      {#if !list.ownOnly}
        <SortHeader
          label="Submitter"
          key="submitter"
          sortKey={list.sortKey}
          sortDir={list.sortDir}
          onsort={list.toggleSort}
        />
      {/if}
      <th scope="col">Progress</th>
      {#if list.ownOnly}<th scope="col"><span class="sr-only">Dismiss</span></th>{/if}
    </tr>
  </thead>
  <tbody>
    {#each list.sorted as r (r.run_id)}
      <tr>
        <td>
          <a href={runTabHref(r.run_id, 'overview')}>{r.run_name || r.run_id}</a>
          {#if r.run_name}<div class="sub mono">{r.run_id}</div>{/if}
        </td>
        <td>
          {#if r.workflow}
            <a href={runVersionHref(r)}>{r.workflow_name ?? r.workflow}</a>
            <div class="sub mono" title={r.workflow}>{r.workflow}</div>
          {:else}
            <span class="sub">--</span>
          {/if}
        </td>
        <td><StatusPill state={r.state} /></td>
        <td>{r.engine ?? '--'}</td>
        <td>{r.project ?? '--'}</td>
        <td>{fmtDate(r.submitted_at)}</td>
        <td>{fmtDate(r.completed_at)}</td>
        <td>{fmtWallTime(r.submitted_at, r.completed_at)}</td>
        <RunCostCell run={r} />
        {#if !list.ownOnly}
          <td title={r.submitter ?? undefined}>{r.submitter_label ?? r.submitter ?? '--'}</td>
        {/if}
        <td>
          <Progress compact run={r} />
        </td>
        {#if list.ownOnly}<DismissCell
            name={list.nameOf(r)}
            ondismiss={() => list.dismissOne(r)}
          />{/if}
      </tr>
    {/each}
  </tbody>
</table>

<style>
  /* A cell's link is its own line above the muted id, not a link inside running text. */
  td > a {
    display: inline-block;
  }
  .sub {
    font-size: 0.72rem;
    color: var(--text-muted);
    margin-top: 0.15rem;
  }
  .sub.mono {
    font-family: var(--mono);
    max-width: 22ch;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .num {
    text-align: right;
    white-space: nowrap;
  }
</style>
