<script lang="ts">
  // Make an experiment: a name (checked as you type against the project's experiments, as the
  // server checks it), an optional description, and — when the caller has not fixed it — the
  // project it lives in. The server's refusal (a name taken meanwhile, no permission) is shown as it
  // words it. Used by the Experiments page, the wizard's Review and the analysis page's menu.
  import { createExperiment, type Experiment } from '$lib/api';
  import { errorMessage } from '$lib/util/format';
  import { view } from '$lib/state/userPrefs.svelte';
  import { t } from '$lib/view/vocab';
  import { NAME_INPUT_MAXLENGTH, nameProblem } from './experiments';

  interface Props {
    /** The projects to choose from; one, or a fixed `project`, shows no choice. */
    projects?: string[];
    /** The project it must live in (an analysis's, a pipeline's). */
    project?: string | null;
    /** The project chosen at first, of `projects`. */
    initialProject?: string | null;
    /** The experiments known so far, for the name check. */
    existing: Experiment[];
    oncreated: (e: Experiment) => void;
    oncancel?: () => void;
    /** Ask for a description too (the inline forms leave it for the experiment's page). */
    withDescription?: boolean;
  }
  let {
    projects = [],
    project = null,
    initialProject = null,
    existing,
    oncreated,
    oncancel,
    withDescription = true,
  }: Props = $props();

  const uid = $props.id();
  let name = $state('');
  let description = $state('');
  // The project picked in the select. It follows `projects` while it names none of them (they may
  // load after the form opens): the initial project when offered, else the first.
  let chosenProject = $state('');
  $effect(() => {
    if (project || projects.includes(chosenProject)) return;
    chosenProject =
      (initialProject && projects.includes(initialProject) ? initialProject : projects[0]) ?? '';
  });
  const inProject = $derived(project ?? chosenProject);
  let touched = $state(false);
  let saving = $state(false);
  let serverError = $state<string | null>(null);
  const problem = $derived(nameProblem(name, inProject, existing));
  // A clash or an over-long name shows as you type; a blank one only once you try to create it.
  const shownProblem = $derived(touched || name.trim() ? problem : null);

  async function submit(e: SubmitEvent) {
    e.preventDefault();
    touched = true;
    serverError = null;
    if (problem || !inProject || saving) return;
    saving = true;
    try {
      const made = await createExperiment({
        project: inProject,
        name: name.trim(),
        ...(withDescription && description.trim() ? { description: description.trim() } : {}),
      });
      oncreated(made);
    } catch (err) {
      serverError = errorMessage(err);
    } finally {
      saving = false;
    }
  }
</script>

<form class="new-exp" onsubmit={submit} aria-label="New experiment" novalidate>
  {#if !project && projects.length > 1}
    <label for={`${uid}-project`}>Project</label>
    <select id={`${uid}-project`} bind:value={chosenProject}>
      {#each projects as p (p)}<option value={p}>{p}</option>{/each}
    </select>
  {/if}
  <label for={`${uid}-name`}>Name</label>
  <input
    id={`${uid}-name`}
    bind:value={name}
    maxlength={NAME_INPUT_MAXLENGTH}
    placeholder="e.g. Liver tumor study"
    aria-invalid={shownProblem ? 'true' : undefined}
    aria-describedby={shownProblem ? `${uid}-problem` : undefined}
    oninput={() => (serverError = null)}
  />
  {#if shownProblem}<p class="problem" id={`${uid}-problem`}>{shownProblem}</p>{/if}
  {#if withDescription}
    <label for={`${uid}-desc`}>Description <span class="opt">(optional)</span></label>
    <textarea id={`${uid}-desc`} rows="2" bind:value={description}></textarea>
  {/if}
  {#if inProject && (project || projects.length <= 1)}
    <p class="hint">In project {inProject}: everyone with access to it can see the experiment.</p>
  {/if}
  {#if !inProject}
    <p class="problem">
      There is no project to put it in yet: a developer registers a {t('workflowLower', view.mode)} first.
    </p>
  {/if}
  {#if serverError}<p class="problem" role="alert">{serverError}</p>{/if}
  <div class="buttons">
    <button type="submit" class="btn btn-primary" disabled={saving}
      >{saving ? 'Creating…' : 'Create experiment'}</button
    >
    {#if oncancel}<button type="button" class="btn" onclick={oncancel}>Cancel</button>{/if}
  </div>
</form>

<style>
  .new-exp {
    display: grid;
    gap: 0.3rem;
    max-width: 30rem;
  }
  label {
    font-size: 0.8rem;
    font-weight: 600;
    margin-top: 0.3rem;
  }
  .opt {
    font-weight: normal;
    color: var(--text-muted);
  }
  input,
  select,
  textarea {
    font: inherit;
    font-size: 0.88rem;
    padding: 0.4rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    min-width: 0;
  }
  input[aria-invalid='true'] {
    border-color: var(--fg-fail);
  }
  .problem {
    margin: 0;
    font-size: 0.8rem;
    color: var(--fg-fail);
  }
  .hint {
    margin: 0.2rem 0 0;
  }
  .buttons {
    display: flex;
    gap: 0.5rem;
    margin-top: 0.5rem;
  }
  input:focus-visible,
  select:focus-visible,
  textarea:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
