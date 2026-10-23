<script lang="ts">
  // The workflow's versions, to switch between; switching keeps the tab.
  import { goto } from '$app/navigation';
  import type { WorkflowSummary } from '#lib/api/index.js';
  import { fmtRelativeMicros } from '#lib/util/format.js';
  import { isReleased } from './release';
  import { versionLabel } from './version';
  import { workflowTabHref, type WorkflowTab } from './workflowNav';

  interface Props {
    versions: WorkflowSummary[];
    /** The version on show. */
    id: string;
    /** The tab on show, which a switch keeps. */
    tab: WorkflowTab;
  }
  let { versions, id, tab }: Props = $props();

  function pickVersion(vid: string) {
    if (vid !== id) goto(workflowTabHref(vid, tab));
  }
</script>

<label class="version-picker">
  <span>Version</span>
  <select value={id} onchange={(e) => pickVersion((e.currentTarget as HTMLSelectElement).value)}>
    {#each versions as v}
      <option value={v.id}>
        {versionLabel(v)} · {fmtRelativeMicros(v.created_at_micros)}{isReleased(v)
          ? ' · released'
          : ''}
      </option>
    {/each}
  </select>
</label>

<style>
  .version-picker {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .version-picker select {
    padding: 0.3rem 0.5rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font-size: 0.8rem;
  }
</style>
