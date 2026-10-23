<script lang="ts">
  // Settings › Site: the header title and logo, shown to everyone. Saves only the fields changed
  // from what is stored; a save shows only this form as stored, so an unsaved edit in the support
  // contact form is kept.
  import { untrack } from 'svelte';
  import { putSettings } from '$lib/api';
  import { errorMessage } from '$lib/util/format';
  import { MAX_DATA_URL_CHARS, MAX_FILE_KB, logoTooLarge, siteChanges } from './settings';
  import type { SiteSettingsState } from './settingsState.svelte';

  let { settings }: { settings: SiteSettingsState } = $props();

  // Shown once the settings have loaded, so the form starts from what is stored.
  let title = $state(untrack(() => settings.stored?.title ?? ''));
  let logo = $state<string | null>(untrack(() => settings.stored?.logo_data_url ?? null));
  let saving = $state(false);
  let error = $state<string | null>(null);
  let saved = $state(false);

  const changes = $derived(siteChanges(settings.stored, title, logo));
  const dirty = $derived(Object.keys(changes).length > 0);

  function onFile(e: Event) {
    error = null;
    saved = false;
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    // Cleared at once, so picking the same file again (after Remove, or a refusal) is a change.
    input.value = '';
    if (!file) return;
    // A file this big can't fit once encoded; don't read it at all.
    if (file.size > MAX_DATA_URL_CHARS) {
      error = logoTooLarge(file.size);
      return;
    }
    const reader = new FileReader();
    reader.onload = () => {
      const url = reader.result as string;
      // The data URL is ASCII, so its length is the byte count the server checks.
      if (url.length > MAX_DATA_URL_CHARS) error = logoTooLarge(file.size);
      else logo = url;
    };
    reader.onerror = () => (error = 'Could not read the image.');
    reader.readAsDataURL(file);
  }

  async function save() {
    if (!settings.stored || !dirty) return;
    saving = true;
    error = null;
    saved = false;
    try {
      const s = await putSettings(changes);
      title = s.title ?? '';
      logo = s.logo_data_url ?? null;
      settings.saved(s);
      saved = true;
    } catch (e) {
      error = errorMessage(e);
    } finally {
      saving = false;
    }
  }
</script>

<div class="form">
  <label>
    <span class="lbl">Title</span>
    <input type="text" bind:value={title} placeholder="biffix" oninput={() => (saved = false)} />
  </label>

  <div class="field">
    <span class="lbl">Logo</span>
    <div class="logo-row">
      {#if logo}
        <img class="preview" src={logo} alt="logo preview" />
        <button
          class="ghost"
          type="button"
          onclick={() => {
            logo = null;
            saved = false;
          }}>Remove</button
        >
      {:else}
        <span class="muted">No logo set</span>
      {/if}
    </div>
    <input
      type="file"
      aria-label="Logo image"
      accept="image/png,image/jpeg,image/gif,image/webp"
      onchange={onFile}
    />
    <span class="hint"
      >PNG, JPEG, GIF or WebP icon, up to about {MAX_FILE_KB} KB. Stored inline.</span
    >
  </div>

  <div class="actions">
    <button onclick={save} disabled={saving || !dirty}>{saving ? 'Saving…' : 'Save'}</button>
    {#if saved}<span class="ok" role="status">Saved ✓</span>{/if}
    {#if error}<span class="error" role="alert">{error}</span>{/if}
  </div>
</div>

<style>
  .logo-row {
    display: flex;
    align-items: center;
    gap: 0.8rem;
  }
  .preview {
    max-height: 48px;
    max-width: 160px;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    padding: 0.2rem;
    background: var(--bg-content);
  }
</style>
