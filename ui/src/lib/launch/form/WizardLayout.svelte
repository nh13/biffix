<script lang="ts">
  // The Lab view's launch wizard: the same form a step at a time, Add samples
  // (2), Reference & options (3) and Review & launch (4), with a stepper above and Back / Next
  // below. Step 1, picking the pipeline, is New run.
  import { page } from '$app/state';
  import { setView } from '$lib/state/userPrefs.svelte';
  import ReviewStep from '../ReviewStep.svelte';
  import WizardStepper from '../WizardStepper.svelte';
  import type { WizardStep } from '../wizard';
  import AdvancedBlock from './AdvancedBlock.svelte';
  import ExamplesMenu from './ExamplesMenu.svelte';
  import FieldControl from './FieldControl.svelte';
  import FormCard from './FormCard.svelte';
  import ProblemList from './ProblemList.svelte';
  import RunDetailsCard from './RunDetailsCard.svelte';
  import SectionCards from './SectionCards.svelte';
  import WizardLaunchCard from './WizardLaunchCard.svelte';
  import { launchForm } from './launchForm.svelte';

  /** The version as the Review names it (e.g. its version label). */
  let { versionText = null }: { versionText?: string | null } = $props();
  const form = launchForm();
</script>

{#if form.schema.fields.length === 0 && !form.loadingRun}
  <section class="card no-form" aria-labelledby="wizard-heading">
    <h2 id="wizard-heading">This pipeline has no launch form</h2>
    <p>
      It declares no parameters, so there is nothing to fill in here.
      <a
        href={page.url.pathname}
        onclick={(e) => {
          e.preventDefault();
          void setView('expert');
        }}>Switch to the Expert view</a
      > to import a parameter file, or to launch it as it is.
    </p>
  </section>
{:else}
  <WizardStepper
    current={form.step}
    hrefFor={form.stepLink}
    done={form.stepDone}
    onnav={(n) => form.goStep(n)}
  />
  {#if form.loadingRun}
    <p class="muted">Loading the earlier analysis…</p>
  {:else if form.formStep === 2}
    <div class="step-head">
      <h2 id="wizard-heading" class="step-title" tabindex="-1">Add samples</h2>
      {#if form.examples.length > 0}<ExamplesMenu align="right" />{/if}
    </div>
    {#if form.examples.length > 0 && !form.hasInput && !form.bannerDismissed}
      <div class="first-time spaced" role="note">
        <span
          >New here? Start from an example to see what a finished analysis looks like, then swap in
          your own samples.</span
        >
        <button class="link" onclick={() => form.setExamplesOpen(true)}>Show the examples</button>
        <button class="close" aria-label="Dismiss" onclick={() => form.dismissBanner()}>×</button>
      </div>
    {/if}
    <FormCard label="Samples">
      {#if form.steps.samples.length === 0}
        <p class="muted">
          This pipeline takes no sample files. Go on to its reference and options.
        </p>
      {:else}
        {#each form.steps.samples as f (f.name)}
          <FieldControl field={f} />
        {/each}
      {/if}
    </FormCard>
  {:else if form.formStep === 3}
    <h2 id="wizard-heading" class="step-title" tabindex="-1">Reference & options</h2>
    <SectionCards cards={form.steps.cards} level={3} />
    {#if form.steps.cards.length === 0 && form.steps.advanced.length === 0 && !form.refs}
      <FormCard>
        <p class="muted">This pipeline has no options to set. Go on to review and launch.</p>
      </FormCard>
    {/if}
    <AdvancedBlock groups={form.steps.advanced} />
  {:else}
    <h2 id="wizard-heading" class="step-title" tabindex="-1">Review & launch</h2>
    {#if form.problems.length > 0}
      <div class="banner warn problems" role="status"><ProblemList list={form.problems} /></div>
    {/if}
    <ReviewStep
      pipeline={{
        title: form.schema.title?.trim() || form.detail.name,
        name: form.detail.name,
        versionText,
      }}
      samples={form.reviewSamples}
      reference={form.reviewRef}
      options={form.reviewChanged}
      stepHref={form.stepHref}
      onnav={form.navTo}
    />
    <RunDetailsCard />
    <WizardLaunchCard />
  {/if}
  <!-- On every step: a Re-run whose pre-fill failed says so wherever it lands. -->
  {#if form.launchError}<p class="launch-error" role="alert">{form.launchError}</p>{/if}
  {#if !form.loadingRun}
    <footer class="wizard-nav">
      {#if form.formStep === 2}
        <a class="btn btn-compact" href={form.newRunHref}>← Pick another pipeline</a>
      {:else}
        <button
          class="btn btn-compact"
          onclick={() => form.goStep((form.formStep - 1) as WizardStep)}>← Back</button
        >
      {/if}
      <div class="problems" id="wizard-problems" aria-live="polite">
        {#if form.formStep !== 4}
          {#if form.blocking.length === 0}
            <span class="ready">✓ {form.formStep === 2 ? 'Samples ready' : 'Options ready'}</span>
          {:else}
            <ProblemList list={form.blocking} />
          {/if}
        {/if}
      </div>
      {#if form.formStep !== 4}
        <button
          class="btn btn-compact btn-primary"
          aria-disabled={form.blocking.length > 0}
          aria-describedby="wizard-problems"
          onclick={() => form.nextStep()}
          >Next: {form.formStep === 2 ? 'Reference & options' : 'Review & launch'} →</button
        >
      {/if}
    </footer>
  {/if}
{/if}

<style>
  .banner {
    padding: 0.55rem 0.8rem;
    border-radius: 0.45rem;
    border: 1px solid var(--border);
    background: var(--hover);
    font-size: 0.85rem;
    margin-bottom: 0.8rem;
  }
  .banner.warn {
    border-left: 3px solid var(--fg-warn);
  }
  .first-time {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.5rem 0.7rem;
    border-radius: 0.45rem;
    border-left: 3px solid var(--accent);
    background: var(--hover);
    font-size: 0.85rem;
  }
  .first-time.spaced {
    margin-bottom: 0.8rem;
  }
  .close {
    border: none;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 1rem;
  }
  .card {
    margin-bottom: 0.9rem;
  }
  .card h2 {
    margin: 0;
    font-size: 1rem;
  }
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .link {
    font-size: 0.82rem;
  }
  .problems {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    font-size: 0.82rem;
  }
  .ready {
    color: var(--fg-ok);
    font-weight: 600;
  }
  .launch-error {
    color: var(--fg-fail);
    font-size: 0.8rem;
  }
  .muted {
    font-size: 0.85rem;
  }
  .step-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    flex-wrap: wrap;
    margin-bottom: 0.6rem;
  }
  .step-title {
    margin: 0 0 0.6rem;
    font-size: 1.15rem;
  }
  .step-head .step-title {
    margin: 0;
  }
  .step-title:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .no-form h2 {
    margin: 0 0 0.4rem;
  }
  .no-form p {
    margin: 0;
    font-size: 0.9rem;
  }
  .no-form a {
    color: var(--accent);
  }
  .wizard-nav {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.8rem 0 0;
    margin-top: 0.2rem;
    border-top: 1px solid var(--border);
  }
  .wizard-nav .problems {
    flex: 1;
    justify-content: center;
  }
  @media (max-width: 800px) {
    .wizard-nav {
      flex-wrap: wrap;
    }
    .wizard-nav .problems {
      order: -1;
      flex-basis: 100%;
      justify-content: flex-start;
    }
  }
</style>
