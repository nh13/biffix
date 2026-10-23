<script lang="ts">
  // A runs list's Cost cell, in either view.
  import type { RunSummary } from '#lib/api/index.js';
  import { listCostLine } from '#lib/runs/cost.js';

  let { run }: { run: RunSummary } = $props();
  const cost = $derived(listCostLine(run));
</script>

<!-- Shown as `—` / `≈ $1.20 so far`; read out in words (`No cost yet`, `about $1.20 so far`). -->
<td class="num cost" title={cost.title}
  ><span aria-hidden="true">{cost.text}</span><span class="sr-only">{cost.sr}</span></td
>

<style>
  .num {
    text-align: right;
    white-space: nowrap;
  }
  .cost {
    font-variant-numeric: tabular-nums;
  }
</style>
