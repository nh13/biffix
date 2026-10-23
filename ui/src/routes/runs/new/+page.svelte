<script lang="ts">
  // New run: pick what to launch. The Lab view picks from the pipeline catalog (step 1 of the
  // launch wizard, whose Start opens the pipeline's wizard at step 2); the Expert view from its
  // workflow list.
  import { page } from '$app/state';
  import { listExperiments, type Experiment } from '#lib/api/index.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { experimentHref } from '#lib/experiments/experiments.js';
  import PipelineCatalog from '#lib/catalog/PipelineCatalog.svelte';
  import WizardStepper from '#lib/launch/WizardStepper.svelte';
  import WorkflowChooser from '#lib/launch/WorkflowChooser.svelte';

  const lab = $derived(view.mode === 'lab');
  // *Start an analysis in this experiment*: carried on to the pipeline's form.
  const experiment = $derived(page.url.searchParams.get('experiment'));
  // That experiment (from the list, archived too: its detail would read every run), for its name
  // and its project, whose pipelines alone are offered. Unknown, it filters nothing (the launch form
  // says it can't launch into it).
  let inExperiment = $state<Experiment | null>(null);
  $effect(() => {
    const id = experiment;
    inExperiment = null;
    if (!id) return;
    let cancelled = false;
    listExperiments({ archived: true })
      .then((l) => {
        if (!cancelled) inExperiment = l.experiments.find((e) => e.id === id) ?? null;
      })
      .catch(() => {
        /* every pipeline is offered */
      });
    return () => {
      cancelled = true;
    };
  });
  const experimentProject = $derived(inExperiment?.project ?? null);
</script>

{#snippet experimentLine()}
  {#if inExperiment}
    <p class="in-experiment" role="status" data-testid="start-in-experiment">
      In the experiment <a href={experimentHref(inExperiment.id)}>{inExperiment.name}</a>: only {lab
        ? 'pipelines'
        : 'workflows'} of its project
      <strong>{inExperiment.project}</strong> are listed.
    </p>
  {/if}
{/snippet}

{#if lab}
  <a class="back-link" href="/runs">← Analyses</a>
  <PipelineCatalog
    {experiment}
    project={experimentProject}
    title="Start an analysis"
    lede="Step 1 of 4 · Pick a pipeline. You'll add your samples next."
  >
    <WizardStepper current={1} />
    {@render experimentLine()}
  </PipelineCatalog>
{:else}
  <WorkflowChooser {experiment} {experimentProject}>
    {@render experimentLine()}
  </WorkflowChooser>
{/if}

<style>
  .in-experiment {
    margin: 0.4rem 0 0.8rem;
    padding: 0.45rem 0.7rem;
    border-left: 3px solid var(--accent);
    background: var(--hover);
    font-size: 0.88rem;
    max-width: 48rem;
  }
</style>
