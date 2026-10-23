<script lang="ts">
  // The run's own details, below its parameters: its name, where results go and how long
  // intermediate files stay. The Expert form's *Run details* adds the params YAML for the CLI; the
  // wizard's Review *Analysis details* adds the experiment to launch into, and folds where results
  // go into one line that still says what it is.
  import NewExperimentForm from '#lib/experiments/NewExperimentForm.svelte';
  import FieldInput from '../FieldInput.svelte';
  import type { Retention } from '../retention';
  import FormCard from './FormCard.svelte';
  import { launchForm, NEW_EXPERIMENT } from './launchForm.svelte';

  const form = launchForm();
  const noun = $derived(form.wizard ? 'analysis' : 'run');
</script>

{#snippet nameRow(label: string, hint: string)}
  <div class="detail-row">
    <label for="run-name">{label}</label>
    <div>
      <input
        id="run-name"
        class="wide"
        maxlength="200"
        bind:value={form.runName}
        oninput={() => (form.dirty = true)}
      />
      <p class="hint">{hint}</p>
    </div>
  </div>
{/snippet}

{#snippet outputRows()}
  <div class="detail-row">
    <span class="detail-label">Output location</span>
    <div>
      {#if form.from?.resume}
        <p class="hint">The resumed {noun}'s folder (a resume shares it).</p>
      {:else if form.lay.outdir}
        {@const outdir = form.lay.outdir}
        <FieldInput
          field={{ ...outdir, label: 'Results folder', required: false }}
          value={form.values[outdir.name]}
          onchange={(v) => form.setValue(outdir.name, v)}
          ontouch={() => form.touch(outdir.name)}
          error={form.errorFor(outdir.name)}
          placeholder={form.outputPlaceholder}
          showName={form.shownAll}
        />
      {:else}
        <p class="hint">This {noun}'s own folder in the project bucket.</p>
      {/if}
    </div>
  </div>
  {#if form.engine === 'nextflow'}
    <div class="detail-row">
      <label for="retention">Intermediate files</label>
      <div>
        <select
          id="retention"
          value={form.retention}
          onchange={(e) => form.setRetention(e.currentTarget.value as Retention)}
        >
          {#each form.retentionChoices as o (o.value)}
            <option value={o.value}>{o.label}</option>
          {/each}
        </select>
        <p class="hint">
          The files steps pass between each other. Results are always kept; {form.wizard
            ? 'an analysis'
            : 'a run'} whose intermediate files are deleted can't be resumed.
        </p>
      </div>
    </div>
  {/if}
{/snippet}

{#if form.wizard}
  <FormCard id="rv-details" title="Analysis details" level={3}>
    {@render nameRow('Analysis name', "Shown on My work and on the analysis's page.")}
    {#if form.project && !form.preview}
      <div class="detail-row">
        {#if form.experiments.creating}
          <!-- The new-experiment form below labels its own fields. -->
          <span class="detail-label">Experiment</span>
        {:else}
          <label for="run-experiment">Experiment</label>
        {/if}
        <div>
          {#if form.experiments.creating}
            <NewExperimentForm
              project={form.project}
              existing={form.experiments.list ?? []}
              withDescription={false}
              oncreated={(e) => form.experimentCreated(e)}
              oncancel={() => (form.experiments.creating = false)}
            />
          {:else}
            <select
              id="run-experiment"
              class="wide"
              value={form.experiment ?? ''}
              disabled={form.experiments.list === null && !form.experiments.error}
              onchange={(e) => form.chooseExperiment(e.currentTarget.value)}
            >
              <option value="">None</option>
              {#each (form.experiments.list ?? []).filter((x) => x.archived_at == null) as x (x.id)}
                <option value={x.id}>{x.name}</option>
              {/each}
              {#if form.experiment && !form.experiments.list?.some((x) => x.id === form.experiment)}
                <option value={form.experiment}>{form.experiment}</option>
              {/if}
              <option value={NEW_EXPERIMENT}>New experiment…</option>
            </select>
            <p class="hint">
              {#if form.experiments.error}Couldn't load this project's experiments: {form
                  .experiments.error}{:else}Group this analysis with others under a name, to read
                their results together.{/if}
            </p>
            {#if form.experiments.unavailable}
              <p class="hint warn-text">
                The experiment this was started from isn't open in project {form.project}; choose
                another, or none.
              </p>
            {/if}
          {/if}
        </div>
      </div>
    {/if}
    <!-- Where results go and how long intermediate files stay: set for you, so collapsed to
         one line that still says what they are. -->
    <details class="review-more" bind:open={form.reviewMoreOpen}>
      <summary>
        <span class="detail-label">Where results go</span>
        <span class="review-more-line">{form.reviewSummary}</span>
        <span class="review-more-change">{form.reviewMoreOpen ? 'Hide' : 'Change'}</span>
      </summary>
      {@render outputRows()}
      <div class="detail-row">
        <span class="detail-label">Notify</span>
        <p class="hint">
          In the app: the bell at the top of the page says when this analysis finishes or needs you.
        </p>
      </div>
    </details>
  </FormCard>
{:else}
  <FormCard id="sec-run-details" title="Run details">
    {@render nameRow('Run name', "Shown in the runs list and on the run's page.")}
    {@render outputRows()}
    <div class="detail-row">
      <span class="detail-label">For the CLI</span>
      <div>
        <button class="link" onclick={() => form.downloadYaml()}>Download params YAML</button>
        <span class="hint inline"
          >· the file <code>biffix runs submit {form.cliVersionId} --params</code>
          accepts</span
        >
      </div>
    </div>
  </FormCard>
{/if}

<style>
  .detail-row {
    display: grid;
    grid-template-columns: 9rem 1fr;
    gap: 0.75rem;
    padding: 0.6rem 0;
    border-bottom: 1px solid var(--border);
    align-items: start;
  }
  .detail-row:last-child {
    border-bottom: none;
  }
  .review-more > summary {
    display: grid;
    grid-template-columns: 9rem 1fr auto;
    gap: 0.75rem;
    align-items: baseline;
    padding: 0.6rem 0;
    cursor: pointer;
    list-style: none;
  }
  .review-more > summary::-webkit-details-marker {
    display: none;
  }
  .review-more > summary:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .review-more-line {
    font-size: 0.82rem;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .review-more-change {
    font-size: 0.82rem;
    color: var(--accent);
    text-decoration: underline;
  }
  .review-more[open] > summary {
    border-bottom: 1px solid var(--border);
  }
  @media (max-width: 40rem) {
    .review-more > summary {
      grid-template-columns: 1fr auto;
    }
    .review-more-line {
      grid-column: 1 / -1;
      grid-row: 2;
    }
  }
  .detail-row label,
  .detail-label {
    font-size: 0.88rem;
    font-weight: 600;
    padding-top: 0.35rem;
  }
  .detail-row :global(.field) {
    padding: 0;
    border: none;
  }
  input.wide,
  select.wide {
    box-sizing: border-box;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    width: min(100%, 30rem);
  }
  .hint {
    margin: 0.25rem 0 0;
  }
  .hint.inline {
    display: inline;
  }
  .hint.warn-text {
    color: var(--fg-warn);
  }
  .link:focus-visible,
  summary:focus-visible {
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
  @media (max-width: 800px) {
    .detail-row {
      grid-template-columns: 1fr;
      gap: 0.25rem;
    }
  }
</style>
