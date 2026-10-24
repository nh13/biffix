<script lang="ts">
  // *Add to experiment* on the run page's header, in both views: put the analysis in one
  // of its project's experiments (or a new one), move it to another, or take it out. An analysis is
  // in at most one. The server checks the permission; its refusal is shown as it words it. A
  // disclosure (`Disclosure`): Escape, or a click or focus outside, closes it.
  import { getRun, listExperiments, setRunExperiment, type Experiment } from '#lib/api/index.js';
  import { Disclosure } from '#lib/util/disclosure.svelte.js';
  import { errorMessage } from '#lib/util/format.js';
  import { getRunContext } from '#lib/runs/runContext.svelte.js';
  import NewExperimentForm from './NewExperimentForm.svelte';
  import { experimentHref } from './experiments';

  let {
    runId,
    project,
    current,
    currentName = $bindable(null),
  }: {
    runId: string;
    project: string | null | undefined;
    /** The experiment it is in now. */
    current: string | null | undefined;
    /** The name of the experiment it is in, once known (for the page's own line). */
    currentName?: string | null;
  } = $props();

  const ctx = getRunContext();
  const uid = $props.id();
  const panelId = `${uid}-exp`;
  const disclosure = new Disclosure();

  // The project's experiments (archived ones too, so the one it is in is named even then).
  let experiments = $state<Experiment[] | null>(null);
  let loadError = $state<string | null>(null);
  let attempt = $state(0);
  $effect(() => {
    const p = project;
    void attempt;
    // Another run's project (the page is reused across runs), or a retry: nothing stale meanwhile.
    experiments = null;
    loadError = null;
    if (!p) return;
    let cancelled = false;
    listExperiments({ project: p, archived: true })
      .then((l) => {
        if (!cancelled) experiments = l.experiments;
      })
      .catch((e) => {
        if (!cancelled) loadError = errorMessage(e);
      });
    return () => {
      cancelled = true;
    };
  });
  const currentExp = $derived(experiments?.find((e) => e.id === current) ?? null);
  $effect(() => {
    currentName = currentExp?.name ?? null;
  });
  const open = $derived((experiments ?? []).filter((e) => e.archived_at == null));
  /** Where it can go: the open experiments other than the one it is in. */
  const choices = $derived(open.filter((e) => e.id !== current));

  let chosen = $state('');
  let creating = $state(false);
  let saving = $state(false);
  let error = $state<string | null>(null);

  function toggle() {
    disclosure.toggle();
    error = null;
    creating = false;
    chosen = '';
  }

  async function setTo(id: string | null) {
    if (saving) return;
    saving = true;
    error = null;
    try {
      await setRunExperiment(runId, id);
      if (ctx.run?.run_id === runId) ctx.run = { ...ctx.run, experiment_id: id };
      disclosure.close({ focus: true });
      // The run's own record, now with its experiment (the poll may have stopped: it ended).
      getRun(runId, { tasks: false })
        .then((r) => {
          if (ctx.run?.run_id === runId) ctx.run = r;
        })
        .catch(() => {
          /* the change is shown already */
        });
    } catch (e) {
      error = errorMessage(e);
    } finally {
      saving = false;
    }
  }
  function created(e: Experiment) {
    experiments = [e, ...(experiments ?? [])];
    creating = false;
    void setTo(e.id);
  }
</script>

<svelte:window onpointerdown={disclosure.onWindowPointer} />

{#if project}
  <!-- svelte-ignore a11y_no_static_element_interactions -->
  <div
    class="add-exp"
    bind:this={disclosure.root}
    onkeydown={disclosure.onkeydown}
    onfocusout={disclosure.onfocusout}
  >
    <button
      type="button"
      class="btn"
      bind:this={disclosure.button}
      aria-expanded={disclosure.open}
      aria-controls={panelId}
      onclick={toggle}
      >{#if current}Experiment <span aria-hidden="true">▾</span>{:else}Add to experiment{/if}</button
    >
    {#if disclosure.open}
      <div class="panel" id={panelId} role="region" aria-label="Experiment">
        {#if current}
          <p class="now">
            In <a href={experimentHref(current)}>{currentExp?.name ?? current}</a>
          </p>
        {/if}
        {#if loadError}
          <p class="error" role="alert">
            Couldn't load the experiments: {loadError}
            <button class="link" onclick={() => attempt++}>Try again</button>
          </p>
        {:else if experiments === null}
          <p class="muted">Loading…</p>
        {:else if creating}
          <NewExperimentForm
            {project}
            existing={experiments}
            withDescription={false}
            oncreated={created}
            oncancel={() => (creating = false)}
          />
        {:else}
          {#if choices.length}
            <label for={`${panelId}-pick`}>{current ? 'Move it to' : 'Add it to'}</label>
            <div class="pick-row">
              <select id={`${panelId}-pick`} bind:value={chosen}>
                <option value="" disabled>Choose an experiment</option>
                {#each choices as e (e.id)}<option value={e.id}>{e.name}</option>{/each}
              </select>
              <button
                type="button"
                class="btn btn-primary"
                disabled={!chosen || saving}
                onclick={() => setTo(chosen)}>{current ? 'Move' : 'Add'}</button
              >
            </div>
          {:else if !current}
            <p class="muted">Project {project} has no experiments yet.</p>
          {/if}
          <div class="more">
            <button type="button" class="link" onclick={() => (creating = true)}
              >New experiment…</button
            >
            {#if current}
              <button
                type="button"
                class="link remove"
                disabled={saving}
                onclick={() => setTo(null)}
                >Remove from {currentExp?.name ?? 'its experiment'}</button
              >
            {/if}
          </div>
        {/if}
        {#if error}<p class="error" role="alert">{error}</p>{/if}
      </div>
    {/if}
  </div>
{/if}

<style>
  .add-exp {
    position: relative;
    display: inline-flex;
  }
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .panel {
    position: absolute;
    top: calc(100% + 0.4rem);
    right: 0;
    z-index: 20;
    width: min(22rem, calc(100vw - 2rem));
    box-sizing: border-box;
    padding: 0.8rem 0.9rem;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
    background: var(--bg-content);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
    display: grid;
    gap: 0.45rem;
  }
  .now {
    margin: 0;
    font-size: 0.85rem;
  }
  .now a {
    color: var(--accent);
    text-decoration: underline;
    font-weight: 600;
  }
  label {
    font-size: 0.8rem;
    font-weight: 600;
  }
  .pick-row {
    display: flex;
    gap: 0.4rem;
  }
  select {
    flex: 1;
    min-width: 0;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.5rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
  }
  .more {
    display: flex;
    flex-wrap: wrap;
    justify-content: space-between;
    gap: 0.4rem 1rem;
    font-size: 0.82rem;
  }
  .link.remove {
    color: var(--fg-fail);
  }
  .muted {
    margin: 0;
    font-size: 0.82rem;
  }
  .error {
    margin: 0;
    font-size: 0.8rem;
  }
  @media (max-width: 640px) {
    .panel {
      right: auto;
      left: 0;
    }
  }
</style>
