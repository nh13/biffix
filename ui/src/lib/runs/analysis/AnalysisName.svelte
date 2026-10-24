<script lang="ts">
  // The analysis page's heading with inline rename: the analysis's name (its own, else one
  // derived from its pipeline, day and samples), and a pencil that turns it into a field. Save
  // records the new name on the server (an event, so it lasts and says who renamed it); clearing
  // the field goes back to the derived name. Enter saves, Escape cancels.
  import { tick } from 'svelte';
  import { renameRun, type RunDetail, type RunSummary } from '#lib/api/index.js';
  import { errorMessage } from '#lib/util/format.js';
  import { derivedName, isDerivedName, runName, runNameSource } from '#lib/runs/runName.js';

  interface Props {
    run: RunDetail;
    /** The server's summary after a rename, for the page to show at once. */
    onrenamed: (summary: RunSummary) => void;
  }
  let { run, onrenamed }: Props = $props();

  /** At most this many characters (the server's limit). */
  const MAX = 200;

  const name = $derived(runName(run));
  let editing = $state(false);
  let draft = $state('');
  let saving = $state(false);
  let error = $state<string | null>(null);
  let input = $state<HTMLInputElement | null>(null);
  let pencil = $state<HTMLButtonElement | null>(null);

  async function start() {
    draft = run.run_name?.trim() ?? '';
    error = null;
    editing = true;
    await tick();
    input?.focus();
    input?.select();
  }

  async function stop() {
    editing = false;
    error = null;
    await tick();
    pencil?.focus();
  }

  async function save() {
    if (saving) return;
    const next = draft.trim();
    if (next === (run.run_name?.trim() ?? '')) return stop();
    saving = true;
    error = null;
    try {
      const summary = await renameRun(run.run_id, next || null);
      onrenamed(summary);
      await stop();
    } catch (e) {
      error = errorMessage(e);
    } finally {
      saving = false;
    }
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Enter') {
      e.preventDefault();
      void save();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      void stop();
    }
  }

  const placeholder = $derived(
    derivedName({ ...runNameSource(run), run_name: null }) ?? run.run_id,
  );
</script>

{#if editing}
  <form
    class="rename"
    onsubmit={(e) => {
      e.preventDefault();
      void save();
    }}
  >
    <label class="sr-only" for="analysis-name-input">Analysis name</label>
    <input
      id="analysis-name-input"
      bind:this={input}
      bind:value={draft}
      maxlength={MAX}
      {placeholder}
      disabled={saving}
      {onkeydown}
      aria-describedby="analysis-name-hint"
    />
    <button type="submit" class="btn btn-primary" disabled={saving}
      >{saving ? 'Saving…' : 'Save'}</button
    >
    <button type="button" class="btn" onclick={stop} disabled={saving}>Cancel</button>
    <span id="analysis-name-hint" class="hint"
      >Leave it empty to name it by its pipeline and date.</span
    >
    {#if error}<span class="error" role="alert">Couldn't rename it: {error}</span>{/if}
  </form>
{:else}
  <div class="name-row">
    <h1 class="name" data-testid="analysis-name" data-derived={isDerivedName(run)}>{name}</h1>
    <button
      bind:this={pencil}
      type="button"
      class="pencil"
      aria-label={`Rename ${name}`}
      title="Rename"
      onclick={start}
      ><svg viewBox="0 0 16 16" width="14" height="14" aria-hidden="true"
        ><path
          d="M11.3 1.7a1 1 0 0 1 1.4 0l1.6 1.6a1 1 0 0 1 0 1.4L5.6 13.4 2 14l.6-3.6z"
          fill="none"
          stroke="currentColor"
          stroke-width="1.4"
          stroke-linejoin="round"
        /></svg
      ></button
    >
  </div>
{/if}

<style>
  .name-row {
    display: flex;
    align-items: center;
    gap: 0.4rem;
  }
  .name {
    margin: 0;
    font-size: 1.3rem;
    overflow-wrap: anywhere;
  }
  .pencil {
    flex: none;
    display: inline-flex;
    padding: 0.25rem;
    border: 1px solid transparent;
    border-radius: 0.35rem;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
  }
  .pencil:hover {
    border-color: var(--control-border);
    color: var(--text);
  }
  .pencil:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .rename {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.4rem;
  }
  input {
    flex: 1 1 16rem;
    min-width: 0;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
    font-size: 1.05rem;
  }
  .hint {
    flex-basis: 100%;
  }
  .error {
    flex-basis: 100%;
    font-size: 0.78rem;
  }
</style>
