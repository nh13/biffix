<script lang="ts">
  // *Import YAML*: a params file (YAML or JSON, the one `biffix runs submit --params` takes), pasted
  // or chosen, filling the form by parameter name. Its card is `.import`, which ⌘/Ctrl+Enter never
  // launches from.
  import { errorMessage } from '$lib/util/format';
  import FormCard from './FormCard.svelte';
  import { launchForm } from './launchForm.svelte';

  const form = launchForm();

  async function importFile(e: Event) {
    const file = (e.currentTarget as HTMLInputElement).files?.[0];
    if (!file) return;
    try {
      form.importText = await file.text();
      form.importError = null;
    } catch (err) {
      form.importError = `${file.name} could not be read: ${errorMessage(err)}`;
    }
  }
</script>

<FormCard class="import" id="import-title" title="Import parameters">
  <p class="muted">
    Paste a params file (YAML or JSON, the same file <code>biffix runs submit --params</code>
    takes) or choose one. Values fill the form by parameter name.
  </p>
  <textarea
    rows="6"
    bind:value={form.importText}
    aria-label="Params YAML"
    placeholder="aligner: star_salmon&#10;min_trimmed_reads: 10000"
  ></textarea>
  <div class="row">
    <input type="file" accept=".yaml,.yml,.json" onchange={importFile} aria-label="Params file" />
    <span class="spacer"></span>
    <button class="btn btn-compact" onclick={() => (form.importOpen = false)}>Cancel</button>
    <button
      class="btn btn-compact btn-primary"
      onclick={() => form.applyImport()}
      disabled={!form.importText.trim()}>Apply</button
    >
  </div>
  {#if form.importError}<p class="error" role="alert">{form.importError}</p>{/if}
</FormCard>

<style>
  .muted {
    font-size: 0.85rem;
  }
  code {
    font-family: var(--mono);
    font-size: 0.78rem;
  }
  textarea {
    width: 100%;
    box-sizing: border-box;
    font-family: var(--mono);
    font-size: 0.8rem;
    padding: 0.4rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    margin-top: 0.4rem;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.5rem;
    flex-wrap: wrap;
  }
  .spacer {
    flex: 1;
  }
  .error {
    color: var(--fg-fail);
    font-size: 0.8rem;
  }
</style>
