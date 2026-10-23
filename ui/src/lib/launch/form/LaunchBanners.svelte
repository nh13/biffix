<script lang="ts">
  // The banners above the form, saying where it came from: a preview of an unregistered form, a
  // Re-run's or Resume's pre-fill, a restored draft, an example (with its undo), an import, and (in
  // the Expert form) the experiment it launches into.
  import { launchForm } from './launchForm.svelte';
  import { runTabHref } from '#lib/runs/runNav.js';

  const form = launchForm();
</script>

{#if form.preview}
  <div class="banner info" role="status">
    <strong>Preview — not registered.</strong> This is the form
    <code>biffix workflows register</code>
    would freeze. Try it out; launching needs a registered version.
  </div>
{/if}
{#if form.from}
  {@const from = form.from}
  <div class="banner info" role="status">
    {#if from.resume}
      Resuming <a href={runTabHref(from.runId, 'overview')}>{from.runId}</a>: finished steps are
      reused, and results go to the same folder.
    {:else}
      Pre-filled from <a href={runTabHref(from.runId, 'overview')}>{from.runId}</a>. Change
      anything, then launch.
    {/if}
    {#if from.images.length}It runs the {from.images.length === 1 ? 'image' : 'images'} that run was submitted
      with again, at the digest it ran: <code>{from.images.join(', ')}</code>.{/if}
    {#if from.secrets.length}
      <strong>Secret parameters must be entered again:</strong> {from.secrets.join(', ')}.
    {/if}
  </div>
{:else if form.notice === 'draft'}
  <div class="banner" role="status">
    Your unsaved form was restored. <button class="link" onclick={() => form.discardDraft()}
      >Discard it</button
    >
  </div>
{:else if form.notice?.startsWith('example:')}
  <div class="banner" role="status">
    Filled from the example <strong>{form.notice.slice('example:'.length)}</strong>. Swap in your
    own inputs, then launch.
    {#if form.beforeExample}<button class="link" onclick={() => form.undoExample()}
        >Undo, and put back what you had entered</button
      >{/if}
  </div>
{:else if form.notice === 'imported'}
  <div class="banner" role="status">Parameters imported.</div>
{:else if form.notice?.startsWith('imported-unknown:')}
  <div class="banner warn" role="status">
    Imported. These keys are not parameters of this version{form.showAll
      ? ' and were kept'
      : ' and were skipped (turn on Show all parameters to keep them)'}: {form.notice.slice(
      'imported-unknown:'.length,
    )}.
  </div>
{/if}
{#if !form.wizard && form.experiment}
  <div class="banner info">
    Launching into the experiment <strong>{form.experimentName}</strong>.
    <button class="link" onclick={() => form.chooseExperiment('')}>Launch outside it</button>
  </div>
{:else if !form.wizard && form.experiments.unavailable}
  <div class="banner warn" role="status" data-testid="experiment-unavailable">
    The experiment this was started from <strong>{form.experiments.unavailable}</strong> isn't open
    in project {form.project}, so this launches outside any experiment.
  </div>
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
  .banner.info {
    border-left: 3px solid var(--accent);
  }
  .banner.warn {
    border-left: 3px solid var(--fg-warn);
  }
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .link {
    font-size: 0.82rem;
  }
  code {
    font-family: var(--mono);
    font-size: 0.78rem;
  }
</style>
