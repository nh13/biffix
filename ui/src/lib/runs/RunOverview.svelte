<script lang="ts">
  // A run's Overview in the Expert view: why it failed, its reports and samples, where it writes
  // and what it read, what needs attention, and its outputs at a glance. The other views are tabs.
  // The Lab view shows its one analysis page (`AnalysisPage`) instead.
  import type { RunDetail, RunReport } from '$lib/api';
  import RunFacts from '$lib/runs/RunFacts.svelte';
  import NeedsAttention from '$lib/runs/NeedsAttention.svelte';
  import LatestOutputs from '$lib/runs/LatestOutputs.svelte';
  import FailureSummary from '$lib/runs/FailureSummary.svelte';
  import ReportsLine from '$lib/runs/ReportsLine.svelte';
  import SampleGrid from '$lib/runs/analysis/SampleGrid.svelte';
  import { isLiveRun } from '$lib/runs/runState';

  let { run, reports }: { run: RunDetail; reports: RunReport[] | null } = $props();
</script>

<FailureSummary {run} />
<ReportsLine runId={run.run_id} {reports} />
<SampleGrid runId={run.run_id} live={isLiveRun(run)} />
<RunFacts {run} />
<div class="columns">
  <NeedsAttention {run} />
  <LatestOutputs {run} />
</div>

<style>
  .columns {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(22rem, 1fr));
    gap: 0 1.5rem;
  }
</style>
