<script lang="ts">
  // Settings → References: the reference bundles launch forms offer. Anyone can
  // look; adding, editing and deleting needs admin (the server says so when it refuses).
  import { deleteReference, listReferences, type ReferenceBundle } from '#lib/api/index.js';
  import { errorMessage } from '#lib/util/format.js';
  import { bundleForm, newBundleForm, type Editing } from '#lib/references/bundleForm.js';
  import BundleEditor from '#lib/references/BundleEditor.svelte';
  import BundleList from '#lib/references/BundleList.svelte';

  let bundles = $state<ReferenceBundle[]>([]);
  let loading = $state(true);
  // The list's first load failed: there is nothing to show but the error.
  let error = $state<string | null>(null);
  // A delete, or the refresh after a save or delete, failed: said above the list, which stays.
  let actionError = $state<string | null>(null);
  let loaded = false;
  // Only the latest load lands: a slow refresh answered after a newer one (a save, then a delete)
  // would bring back what the newer one no longer lists.
  let latest = 0;

  async function load() {
    const seq = ++latest;
    try {
      const listed = await listReferences();
      if (seq !== latest) return;
      bundles = listed;
      loaded = true;
      error = null;
    } catch (e) {
      if (seq !== latest) return;
      const msg = errorMessage(e);
      if (loaded) actionError = `Couldn't refresh the list: ${msg}`;
      else error = msg;
    } finally {
      if (seq === latest) loading = false;
    }
  }
  $effect(() => {
    load();
  });

  let editing = $state<Editing | null>(null);
  let savedNote = $state<{ id: string; warnings: string[] } | null>(null);

  function startEditing(form: Editing) {
    editing = form;
    actionError = null;
    savedNote = null;
  }
  async function saved(note: { id: string; warnings: string[] }) {
    savedNote = note;
    editing = null;
    await load();
  }

  let confirmDelete = $state<string | null>(null);
  async function remove(id: string) {
    if (confirmDelete !== id) {
      confirmDelete = id;
      return;
    }
    confirmDelete = null;
    actionError = null;
    try {
      await deleteReference(id);
    } catch (e) {
      actionError = `Couldn't delete ${id}: ${errorMessage(e)}`;
      return;
    }
    savedNote = null;
    await load();
  }
</script>

<a class="back-link" href="/settings">← Settings</a>
<h1>References</h1>
<p class="lede">
  Named sets of reference files — a genome's FASTA, its indexes, its annotation — that launch forms
  offer in their reference dropdown. biffix stores only the paths; the files stay where they are.
</p>

{#if savedNote}
  <div class="banner" class:warn={savedNote.warnings.length > 0} role="status">
    Saved <strong>{savedNote.id}</strong>.
    {#if savedNote.warnings.length > 0}
      Some files were not found (they may still be on their way):
      <ul>
        {#each savedNote.warnings as w}<li>{w}</li>{/each}
      </ul>
    {/if}
  </div>
{/if}

{#if editing}
  <!-- Afresh for each bundle (its error and file picker are that bundle's). -->
  {#key editing}<BundleEditor
      bind:editing
      onsaved={saved}
      oncancel={() => (editing = null)}
    />{/key}
{:else}
  <button class="btn btn-primary add" onclick={() => startEditing(newBundleForm())}
    >Add a reference</button
  >
{/if}

{#if actionError}<p class="error" role="alert">{actionError}</p>{/if}

{#if loading}
  <p class="muted">Loading…</p>
{:else if error}
  <p class="error" role="alert">Couldn't load the references: {error}</p>
  <button
    class="btn"
    onclick={() => {
      loading = true;
      load();
    }}>Retry</button
  >
{:else if bundles.length === 0}
  <p class="muted">No references yet.</p>
{:else}
  <BundleList
    {bundles}
    {confirmDelete}
    onedit={(b) => startEditing(bundleForm(b))}
    ondelete={remove}
  />
{/if}

<style>
  h1 {
    margin: 0;
  }
  .lede {
    color: var(--text-muted);
    margin: 0.3rem 0 1rem;
    max-width: 48rem;
  }
  .add {
    margin-bottom: 1rem;
  }
  .banner {
    max-width: 52rem;
    padding: 0.6rem 0.8rem;
    border-left: 3px solid var(--fg-ok);
    background: var(--hover);
    border-radius: 0.3rem;
    margin-bottom: 1rem;
    font-size: 0.88rem;
  }
  .banner.warn {
    border-left-color: var(--fg-warn);
  }
  .banner ul {
    margin: 0.3rem 0 0;
    padding-left: 1.2rem;
    font-family: var(--mono);
    font-size: 0.78rem;
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .error {
    font-size: 0.85rem;
  }
</style>
