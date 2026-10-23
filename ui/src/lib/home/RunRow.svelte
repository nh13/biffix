<script lang="ts">
  // One analysis as a Lab-view row: its name and pipeline, its plain status, and Open (and Dismiss,
  // in Needs attention).
  import type { RunSummary } from '#lib/api/index.js';
  import { fmtRelative } from '#lib/util/format.js';
  import { pipelineTitle, runName } from '#lib/runs/runName.js';
  import { runTabHref } from '#lib/runs/runNav.js';
  import StatusPill from '#lib/runs/StatusPill.svelte';
  import WorkItem from './WorkItem.svelte';
  import { statusLine } from '#lib/runs/status.js';

  let { run, onDismiss }: { run: RunSummary; onDismiss?: () => void } = $props();
  const line = $derived(statusLine(run));
</script>

<WorkItem
  href={runTabHref(run.run_id, 'overview')}
  name={runName(run)}
  tone={line.tone}
  {onDismiss}
>
  {#snippet sub()}{pipelineTitle(run) ??
      'Unknown pipeline'}{#if run.submitted_at != null}{' · started '}{fmtRelative(
        run.submitted_at,
      )}{/if}{/snippet}
  {#snippet middle()}<StatusPill {line} />{/snippet}
</WorkItem>
