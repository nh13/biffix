<script lang="ts">
  import { fmtRelativeMicros } from '#lib/util/format.js';
  import { versionLabel, registrantLabel as who } from '#lib/workflows/version.js';
  import { getWorkflowContext } from '#lib/workflows/workflowContext.svelte.js';
  import { workflowTabHref } from '#lib/workflows/workflowNav.js';
  import ReleaseBadge from '#lib/workflows/ReleaseBadge.svelte';

  const wf = getWorkflowContext();
  const id = $derived(wf.detail?.id ?? '');
</script>

{#if wf.versionsError}
  <p class="error" role="alert">Couldn't load this workflow's versions: {wf.versionsError}</p>
{:else if wf.versionsLoading}
  <p class="muted">Loading versions…</p>
{:else}
  <table class="versions">
    <thead>
      <tr>
        <th>Version</th>
        <th>Registered</th>
        <th>By</th>
        <th>Engine</th>
        <th>Runs</th>
      </tr>
    </thead>
    <tbody>
      {#each wf.versions as v (v.id)}
        <tr class:current={v.id === id}>
          <td>
            <a href={workflowTabHref(v.id, 'versions')} class="mono">{versionLabel(v)}</a>
            <ReleaseBadge released={v.release?.released} />
            {#if v.id === id}<span class="badge">current</span>{/if}
          </td>
          <td>{fmtRelativeMicros(v.created_at_micros)}</td>
          <td>{who(v) ?? '--'}</td>
          <td>{v.engine_version || '--'}</td>
          <td>
            {#if v.run_count > 0}
              <a href={`/runs?workflow=${encodeURIComponent(v.id)}`}>{v.run_count}</a>
            {:else}
              0
            {/if}
          </td>
        </tr>
      {/each}
    </tbody>
  </table>
{/if}

<style>
  table.versions {
    width: 100%;
    border-collapse: collapse;
    font-size: 0.82rem;
  }
  table.versions th {
    text-align: left;
    color: var(--text-muted);
    font-weight: 500;
    padding: 0.35rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  table.versions td {
    padding: 0.4rem 0.5rem;
    border-bottom: 1px solid var(--border);
  }
  tr.current {
    background: var(--bg-content);
  }
  .badge {
    font-size: 0.64rem;
    padding: 0.02rem 0.35rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
    margin-left: 0.35rem;
  }
  .mono {
    font-size: 0.78rem;
  }
</style>
