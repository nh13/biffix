<script lang="ts">
  // The Lab wizard's Review step summary: what the launch will run with, as
  // cards for the pipeline, the samples, the reference and the changed options, each linking back
  // to the step that sets it. It only shows what it is given; the form's state stays in LaunchForm,
  // and wizard.ts works out the samples, reference and options cards' contents.
  import type { ReviewOption, ReviewReference, ReviewSampleLine, WizardStep } from './wizard';
  import { plural } from '#lib/util/format.js';

  interface Props {
    /** The pipeline's title (its schema's, else its name), its name and its version as named. */
    pipeline: { title: string; name: string; versionText: string | null };
    samples: ReviewSampleLine[];
    /** The reference card (`null`: the form has no reference dropdown). */
    reference: ReviewReference | null;
    /** The options changed from a fresh form's. */
    options: ReviewOption[];
    /** A step's link (step 1, picking the pipeline, is New run). */
    stepHref: (n: WizardStep) => string;
    /** A step link's click (a plain one moves in place, a new-tab one opens the link). */
    onnav: (e: MouseEvent, n: WizardStep) => void;
  }
  let { pipeline, samples, reference, options, stepHref, onnav }: Props = $props();
</script>

<div class="review-grid">
  <section class="card review" aria-labelledby="rv-pipeline">
    <div class="rv-head">
      <h3 id="rv-pipeline">Pipeline</h3>
      <a href={stepHref(1)} onclick={(e) => onnav(e, 1)}
        >Change<span class="sr-only"> pipeline</span></a
      >
    </div>
    <p class="rv-main">{pipeline.title}</p>
    <p class="rv-sub">{pipeline.name}{pipeline.versionText ? ` · ${pipeline.versionText}` : ''}</p>
  </section>
  <section class="card review" aria-labelledby="rv-samples">
    <div class="rv-head">
      <h3 id="rv-samples">Samples</h3>
      <a href={stepHref(2)} onclick={(e) => onnav(e, 2)}
        >Edit<span class="sr-only"> samples</span></a
      >
    </div>
    {#each samples as line (line.name)}
      {#if line.kind === 'rows'}
        <p class="rv-main">
          {plural(line.count, 'sample')}
        </p>
        {#if line.first.length > 0}
          <p class="rv-sub">
            {line.first.join(', ')}{line.more > 0 ? ` and ${line.more} more` : ''}
          </p>
        {/if}
      {:else}
        {#if line.sheetFile}<p class="rv-main">From a samplesheet file</p>{/if}
        <p class="rv-sub">
          <span class="rv-label">{line.label}:</span>
          {#if line.value}<code>{line.value}</code>{:else}<span class="muted">not set</span>{/if}
        </p>
      {/if}
    {:else}
      <p class="rv-sub">No sample files.</p>
    {/each}
  </section>
  {#if reference}
    <section class="card review" aria-labelledby="rv-reference">
      <div class="rv-head">
        <h3 id="rv-reference">{reference.label}</h3>
        <a href={stepHref(3)} onclick={(e) => onnav(e, 3)}
          >Edit<span class="sr-only"> reference</span></a
        >
      </div>
      <p class="rv-main">{reference.name}</p>
      {#if reference.detail}<p class="rv-sub">{reference.detail}</p>{/if}
      {#each reference.files as file (file.name)}
        <p class="rv-sub">
          <span class="rv-label">{file.label}:</span>
          {#if file.value}<code>{file.value}</code>{:else}<span class="muted">not set</span>{/if}
        </p>
      {/each}
    </section>
  {/if}
  <section class="card review" aria-labelledby="rv-options">
    <div class="rv-head">
      <h3 id="rv-options">Options</h3>
      <a href={stepHref(3)} onclick={(e) => onnav(e, 3)}
        >Edit<span class="sr-only"> options</span></a
      >
    </div>
    {#if options.length === 0}
      <p class="rv-main">Standard settings</p>
      <p class="rv-sub">Nothing changed from the pipeline's standard settings.</p>
    {:else}
      <ul class="rv-options" aria-label="Changed options">
        {#each options as o (o.name)}
          <li><span class="rv-label">{o.label}:</span> {o.value}</li>
        {/each}
      </ul>
      <p class="rv-sub">Everything else at the pipeline's standard settings.</p>
    {/if}
  </section>
</div>

<style>
  /* The same card, muted text and code as the Run form's (form/FormCard.svelte and the form's
     parts): .card is per-component, so a change to these rules there or here needs the same change
     in the other. */
  .card {
    margin-bottom: 0.9rem;
  }
  .card h3 {
    margin: 0;
    font-size: 0.98rem;
  }
  .muted {
    font-size: 0.85rem;
  }
  code {
    font-family: var(--mono);
    font-size: 0.78rem;
  }
  .review-grid {
    display: grid;
    grid-template-columns: repeat(auto-fit, minmax(min(100%, 22rem), 1fr));
    gap: 0 0.9rem;
  }
  .rv-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 0.5rem;
    margin-bottom: 0.3rem;
  }
  .rv-head h3 {
    font-size: 0.75rem;
    letter-spacing: 0.05em;
    text-transform: uppercase;
    color: var(--text-muted);
  }
  .rv-head a {
    color: var(--accent);
    font-size: 0.82rem;
  }
  .rv-main {
    margin: 0;
    font-weight: 600;
  }
  .rv-sub {
    margin: 0.15rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .rv-label {
    color: var(--text);
  }
  .rv-options {
    margin: 0;
    padding: 0;
    list-style: none;
    font-size: 0.88rem;
  }
</style>
