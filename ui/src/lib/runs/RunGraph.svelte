<script lang="ts">
  import type { RunDetail } from '$lib/api';
  import Dag from '$lib/runs/dag/Dag.svelte';

  // The run's step graph (the caller shows it only when one was recorded); clicking a node picks
  // that group (rule/process).
  interface Props {
    dag: NonNullable<RunDetail['dag']>;
    onselect: (group: string | null) => void;
    /** What picking a node does, for the hint under the heading. */
    hint: string;
  }
  let { dag, onselect, hint }: Props = $props();

  const edges = $derived(dag.edges.map((e) => [e[0], e[1]] as [string, string]));
</script>

<section class="dag-section">
  <h2>Graph</h2>
  <p class="muted dag-hint">{hint}</p>
  <Dag nodes={dag.nodes} {edges} {onselect} />
</section>

<style>
  .dag-section {
    margin: 1rem 0;
  }
  .dag-hint {
    font-size: 0.78rem;
    margin: 0.25rem 0 0.5rem;
  }
</style>
