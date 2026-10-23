<script lang="ts">
  // What a pipeline gives and takes, for a biologist picking one: the key
  // results it declares, its main inputs, and how long (and how much) a run usually takes from its
  // past runs. On a catalog card and the pipeline's page. Lines with nothing to say are left out.
  // The typical run comes with the workflow list (`WorkflowVersionSummary.typical`, one grouped read
  // for every pipeline), so a card asks the server nothing of its own.
  import type { CatalogEntry, WorkflowEstimate } from '#lib/api/index.js';
  import { typicalLine } from '#lib/launch/estimate.js';

  interface Props {
    catalog: CatalogEntry | null | undefined;
    /** The pipeline's typical run, from the workflow list; none, and no time-and-cost line. */
    typical?: WorkflowEstimate['typical'] | null;
    compact?: boolean;
  }
  let { catalog, typical = null, compact = false }: Props = $props();

  const results = $derived(catalog?.results ?? []);
  const inputs = $derived(catalog?.inputs ?? []);
  const usual = $derived(typicalLine(typical));
</script>

{#if results.length || inputs.length || usual}
  <dl class="get" class:compact data-testid="what-you-get">
    {#if results.length}
      <div>
        <dt>You get</dt>
        <dd>{results.join(' · ')}</dd>
      </div>
    {/if}
    {#if inputs.length}
      <div>
        <dt>You give</dt>
        <dd>{inputs.join(' · ')}</dd>
      </div>
    {/if}
    {#if usual}
      <div>
        <dt>Time and cost</dt>
        <dd>{usual}</dd>
      </div>
    {/if}
  </dl>
{/if}

<style>
  .get {
    display: grid;
    gap: 0.2rem;
    margin: 0;
    font-size: 0.8rem;
  }
  .get div {
    display: flex;
    gap: 0.4rem;
    min-width: 0;
  }
  dt {
    flex: none;
    min-width: 5.5rem;
    color: var(--text-muted);
  }
  dd {
    margin: 0;
    min-width: 0;
    overflow-wrap: anywhere;
  }
  .compact {
    font-size: 0.76rem;
  }
  .compact dt {
    min-width: 4.8rem;
  }
</style>
