<script lang="ts">
  // Settings → References: the form that adds or edits one reference bundle, a file picker for each
  // of its paths. The server checks the files and says which it could not find.
  import { saveReference } from '#lib/api/index.js';
  import { errorMessage } from '#lib/util/format.js';
  import FilePicker from '#lib/launch/FilePicker.svelte';
  import { COMMON_KEYS, KINDS, pickerMode, requestOf, type Editing } from './bundleForm';

  interface Props {
    /** The form, edited in place. */
    editing: Editing;
    /** Saved: its id and any files the server could not find. */
    onsaved: (note: { id: string; warnings: string[] }) => Promise<void>;
    oncancel: () => void;
  }
  let { editing = $bindable(), onsaved, oncancel }: Props = $props();

  let saving = $state(false);
  let formError = $state<string | null>(null);
  let pickerRow = $state<number | null>(null);

  async function save() {
    const req = requestOf(editing);
    if (typeof req === 'string') {
      formError = req;
      return;
    }
    saving = true;
    formError = null;
    try {
      const saved = await saveReference(req, editing.original !== null);
      await onsaved({ id: saved.bundle.id, warnings: saved.warnings });
    } catch (e) {
      formError = errorMessage(e);
    } finally {
      saving = false;
    }
  }
</script>

<section class="card editor" aria-labelledby="editor-title">
  <h2 id="editor-title">{editing.original ? `Edit ${editing.original}` : 'Add a reference'}</h2>
  <div class="grid">
    <label for="ref-id">Id</label>
    <div>
      <input
        id="ref-id"
        bind:value={editing.id}
        disabled={editing.original !== null}
        placeholder="grch38-ensembl110"
        autocomplete="off"
        aria-describedby="ref-id-hint"
      />
      <p class="hint" id="ref-id-hint">
        What workflows and examples name it by: up to 64 lowercase letters, digits, <code>-</code>,
        <code>_</code>, <code>.</code>, starting with a letter or digit{#if editing.original}. It
          cannot change; add a new reference instead{/if}.
      </p>
    </div>
    <label for="ref-name">Name</label>
    <input
      id="ref-name"
      bind:value={editing.displayName}
      placeholder="Human GRCh38 · Ensembl 110"
    />
    <label for="ref-kind">Kind</label>
    <select id="ref-kind" bind:value={editing.kind}>
      {#each KINDS as k}<option value={k.value}>{k.label}</option>{/each}
      <!-- A kind this UI does not know (a newer server's) stays selectable as itself. -->
      {#if !KINDS.some((k) => k.value === editing.kind)}<option value={editing.kind}
          >{editing.kind}</option
        >{/if}
    </select>
    <label for="ref-notes">Notes</label>
    <input id="ref-notes" bind:value={editing.notes} placeholder="Optional" />
    <label for="ref-igv">Hosted genome</label>
    <div>
      <input
        id="ref-igv"
        bind:value={editing.igvGenome}
        placeholder="Optional, e.g. hg38 or mm39"
        autocomplete="off"
        aria-describedby="ref-igv-hint"
      />
      <p class="hint" id="ref-igv-hint">
        Show alignments against the genome browser's own copy of this assembly (sequence and genes)
        instead of these files. The browser then loads it from igv.org, so use it only for public
        assemblies.
      </p>
    </div>
  </div>

  <h3>Files</h3>
  <p class="hint">
    Each file under the key a workflow asks for. An index is given by its folder (ending in
    <code>/</code>) or its base path, the name its files share before their extensions:
    <code>…/genome</code> for <code>genome.amb</code> and <code>genome.bwt</code>.
  </p>
  <datalist id="common-keys">
    {#each COMMON_KEYS as k}<option value={k}></option>{/each}
  </datalist>
  <div class="rows">
    {#each editing.rows as row, i}
      <div class="row">
        <input
          class="key"
          aria-label={`Key ${i + 1}`}
          list="common-keys"
          bind:value={row.key}
          placeholder="fasta"
        />
        <input
          class="uri"
          aria-label={`Path ${i + 1}`}
          bind:value={row.uri}
          placeholder="s3://bucket/path/genome.fa"
        />
        <button type="button" class="btn btn-compact" onclick={() => (pickerRow = i)}
          >Browse<span class="sr-only"> for file {i + 1}</span></button
        >
        <button
          type="button"
          class="icon"
          aria-label={`Remove file ${i + 1}`}
          onclick={() => (editing.rows = editing.rows.filter((_, j) => j !== i))}>×</button
        >
      </div>
    {/each}
  </div>
  <button
    type="button"
    class="link"
    onclick={() => (editing.rows = [...editing.rows, { key: '', uri: '' }])}>+ Add a file</button
  >

  {#if formError}<p class="error" role="alert">{formError}</p>{/if}
  <div class="actions">
    <button class="btn btn-primary" onclick={save} disabled={saving}
      >{saving ? 'Saving…' : 'Save'}</button
    >
    <button class="btn" onclick={oncancel} disabled={saving}>Cancel</button>
  </div>
</section>

{#if pickerRow !== null}
  {@const row = editing.rows[pickerRow]}
  <FilePicker
    mode={row ? pickerMode(row) : 'file'}
    label={row?.key || 'reference file'}
    current={row?.uri || null}
    onpick={(uris) => {
      if (pickerRow !== null) editing.rows[pickerRow].uri = uris[0];
      pickerRow = null;
    }}
    onclose={() => (pickerRow = null)}
  />
{/if}

<style>
  .card {
    padding: 1rem 1.1rem;
    margin-bottom: 1rem;
    max-width: 52rem;
  }
  .card h2 {
    margin: 0 0 0.75rem;
    font-size: 1.05rem;
  }
  .card h3 {
    margin: 1rem 0 0.2rem;
    font-size: 0.92rem;
  }
  .grid {
    display: grid;
    grid-template-columns: 6rem minmax(0, 1fr);
    gap: 0.6rem 0.9rem;
    align-items: start;
  }
  .grid label {
    font-size: 0.85rem;
    font-weight: 600;
    padding-top: 0.4rem;
  }
  input,
  select {
    box-sizing: border-box;
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    width: min(100%, 28rem);
  }
  input:disabled {
    color: var(--text-muted);
  }
  input:focus-visible,
  select:focus-visible,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .hint {
    margin: 0.25rem 0 0;
  }
  .rows {
    display: grid;
    gap: 0.4rem;
    margin: 0.6rem 0 0.4rem;
  }
  .row {
    display: flex;
    gap: 0.4rem;
    align-items: center;
    flex-wrap: wrap;
  }
  .row .key {
    width: 11rem;
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .row .uri {
    flex: 1 1 16rem;
    width: auto;
    font-family: var(--mono);
    font-size: 0.8rem;
  }
  .icon {
    border: none;
    background: none;
    font-size: 1.1rem;
    color: var(--text-muted);
    cursor: pointer;
    padding: 0 0.3rem;
  }
  .link {
    text-decoration: none;
    font-size: 0.85rem;
  }
  .actions {
    margin-top: 0.9rem;
  }
  .error {
    font-size: 0.85rem;
  }
  code {
    font-family: var(--mono);
    font-size: 0.85em;
  }
  @media (max-width: 640px) {
    .grid {
      grid-template-columns: 1fr;
    }
    .grid label {
      padding-top: 0;
    }
  }
</style>
