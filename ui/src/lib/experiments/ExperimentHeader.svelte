<script lang="ts">
  // An experiment's header: its name and description (editable in place), who made it and where,
  // Start an analysis in it, its samples as a CSV, and Archive/Restore.
  import {
    getExperimentSamplesCsv,
    updateExperiment,
    type Experiment,
    type ExperimentDetail,
    type ExperimentUpdateRequest,
  } from '#lib/api/index.js';
  import { saveBlob } from '#lib/util/download.js';
  import { errorMessage, fmtDate } from '#lib/util/format.js';
  import type { ViewMode } from '#lib/view/viewRoutes.js';
  import { t } from '#lib/view/vocab.js';
  import {
    NAME_INPUT_MAXLENGTH,
    nameProblem,
    samplesCsvAbout,
    samplesCsvFilename,
    samplesCsvUnavailable,
    withExperiment,
  } from './experiments';

  interface Props {
    detail: ExperimentDetail;
    mode: ViewMode;
    /** An edit was saved: the experiment as it is now. */
    onupdated: (experiment: Experiment) => void;
  }
  let { detail, mode, onupdated }: Props = $props();

  const exp = $derived(detail.experiment);
  const lab = $derived(mode === 'lab');
  const archived = $derived(exp.archived_at != null);
  const startHref = $derived(withExperiment('/runs/new', exp.id));

  // -- Editing the name and description in place --------------------------------------------------
  let editing = $state(false);
  let draftName = $state('');
  let draftDescription = $state('');
  let saving = $state(false);
  let saveError = $state<string | null>(null);
  const draftProblem = $derived(nameProblem(draftName, exp.project, [], exp.id));
  function startEdit() {
    draftName = exp.name;
    draftDescription = exp.description ?? '';
    saveError = null;
    editing = true;
  }
  async function save(update: ExperimentUpdateRequest): Promise<boolean> {
    if (saving) return false;
    saving = true;
    saveError = null;
    try {
      onupdated(await updateExperiment(exp.id, update));
      return true;
    } catch (e) {
      saveError = errorMessage(e);
      return false;
    } finally {
      saving = false;
    }
  }
  async function saveEdit(e: SubmitEvent) {
    e.preventDefault();
    if (draftProblem) return;
    const update: ExperimentUpdateRequest = {};
    if (draftName.trim() !== exp.name) update.name = draftName.trim();
    const desc = draftDescription.trim();
    if (desc !== (exp.description ?? '')) update.description = desc || null;
    if (Object.keys(update).length === 0 || (await save(update))) editing = false;
  }

  // -- Samples as a CSV: every sample of every run (the server reads them all, not only the newest
  // runs the detail holds), saved as a file -------------------------------------------------------
  const csvUnavailable = $derived(samplesCsvUnavailable(detail, mode));
  const csvAbout = $derived(csvUnavailable ?? samplesCsvAbout(mode));
  let savingCsv = $state(false);
  let csvError = $state<string | null>(null);
  async function downloadSamplesCsv() {
    if (savingCsv || csvUnavailable) return;
    savingCsv = true;
    csvError = null;
    try {
      saveBlob(await getExperimentSamplesCsv(exp.id), samplesCsvFilename(exp.name));
    } catch (e) {
      csvError = `Could not download the samples: ${errorMessage(e)}`;
    } finally {
      savingCsv = false;
    }
  }
</script>

<header class="head">
  <div class="ident">
    {#if editing}
      <form class="edit" onsubmit={saveEdit} aria-label="Edit experiment" novalidate>
        <label for="exp-name">Name</label>
        <input
          id="exp-name"
          bind:value={draftName}
          maxlength={NAME_INPUT_MAXLENGTH}
          aria-invalid={draftProblem ? 'true' : undefined}
          aria-describedby={draftProblem ? 'exp-name-problem' : undefined}
        />
        {#if draftProblem}<p class="problem" id="exp-name-problem">{draftProblem}</p>{/if}
        <label for="exp-desc">Description</label>
        <textarea id="exp-desc" rows="3" bind:value={draftDescription}></textarea>
        <div class="buttons">
          <button type="submit" class="btn btn-primary" disabled={saving}
            >{saving ? 'Saving…' : 'Save'}</button
          >
          <button type="button" class="btn" onclick={() => (editing = false)}>Cancel</button>
        </div>
      </form>
    {:else}
      <h1>{exp.name}</h1>
      {#if exp.description}<p class="desc">{exp.description}</p>{/if}
    {/if}
    <p class="sub">
      Created {fmtDate(
        exp.created_at,
      )}{#if exp.created_by_label || exp.created_by}{' by '}{exp.created_by_label ??
          exp.created_by}{/if}
      · project {exp.project}{#if !lab}{' · '}<span class="mono">{exp.id}</span>{/if}
      {#if !editing}
        · <button class="link" onclick={startEdit}
          >Edit<span class="sr-only"> name and description</span></button
        >
      {/if}
    </p>
  </div>
  <div class="actions">
    {#if !archived}
      <a class="btn btn-primary" href={startHref}
        >{lab ? 'Start an analysis in this experiment →' : 'New run in this experiment →'}</a
      >
    {/if}
    <!-- aria-disabled, not disabled, so keyboard and screen-reader users still reach the button
         and hear why (its description); the click does nothing then. -->
    <button
      class="btn"
      onclick={downloadSamplesCsv}
      aria-disabled={!!csvUnavailable || savingCsv}
      aria-busy={savingCsv}
      title={csvAbout}
      aria-describedby="samples-csv-about"
      >{savingCsv ? 'Preparing…' : 'Download samples (CSV)'}</button
    >
    <span class="sr-only" id="samples-csv-about">{csvAbout}</span>
    <span class="sr-only" role="status">{savingCsv ? 'Preparing the samples file…' : ''}</span>
    <button class="btn" onclick={() => save({ archived: !archived })} disabled={saving}
      >{archived ? 'Restore' : 'Archive'}</button
    >
  </div>
</header>
{#if saveError}<p class="error" role="alert">{saveError}</p>{/if}
{#if csvError}<p class="error" role="alert">{csvError}</p>{/if}
{#if archived}
  <p class="archived-note" role="status">
    Archived {fmtDate(exp.archived_at)}: it is hidden from the list of experiments. Restore it to
    start {t('runsLower', mode)} in it again.
  </p>
{/if}

<style>
  .head {
    display: flex;
    flex-wrap: wrap;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.8rem 1.2rem;
    margin-bottom: 1rem;
  }
  .ident {
    min-width: 0;
    flex: 1 1 22rem;
  }
  h1 {
    margin: 0;
    overflow-wrap: anywhere;
  }
  .desc {
    margin: 0.3rem 0 0;
    color: var(--text-muted);
    max-width: 50rem;
    white-space: pre-line;
    overflow-wrap: anywhere;
  }
  .sub {
    margin: 0.4rem 0 0;
    font-size: 0.8rem;
    color: var(--text-muted);
    overflow-wrap: anywhere;
  }
  .edit {
    display: grid;
    gap: 0.3rem;
    max-width: 36rem;
  }
  .edit label {
    font-size: 0.8rem;
    font-weight: 600;
  }
  .edit input,
  .edit textarea {
    font: inherit;
    padding: 0.4rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    min-width: 0;
  }
  .edit input {
    font-size: 1.1rem;
    font-weight: 600;
  }
  .edit input[aria-invalid='true'] {
    border-color: var(--fg-fail);
  }
  .problem {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-fail);
  }
  .buttons {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.3rem;
  }
  .archived-note {
    margin: 0 0 1rem;
    padding: 0.6rem 0.9rem;
    border: 1px dashed var(--card-border);
    border-radius: 0.5rem;
    font-size: 0.85rem;
    color: var(--text-muted);
  }
</style>
