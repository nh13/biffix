<script lang="ts">
  // Settings › Site › Support contact: who a Lab user asks when an analysis needs attention (the
  // *Ask* button on a failed analysis), unless its pipeline names its own contact. Saved on its
  // own; only an Admin may change it (the server says so, and that refusal is shown).
  import { untrack } from 'svelte';
  import { putSettings, type SiteSettings } from '$lib/api';
  import { errorMessage } from '$lib/util/format';
  import { supportChanged } from './settings';
  import type { SiteSettingsState } from './settingsState.svelte';

  let { settings }: { settings: SiteSettingsState } = $props();

  // Shown once the settings have loaded, so the form starts from what is stored.
  let name = $state('');
  let email = $state('');
  let url = $state('');
  function showStored(s: SiteSettings | null) {
    name = s?.support?.name ?? '';
    email = s?.support?.email ?? '';
    url = s?.support?.url ?? '';
  }
  untrack(() => showStored(settings.stored));

  let saving = $state(false);
  let error = $state<string | null>(null);
  let saved = $state<string | null>(null);
  const dirty = $derived(supportChanged(settings.stored, { name, email, url }));
  function edited() {
    saved = null;
  }
  async function save(clear: boolean) {
    if (!settings.stored) return;
    saving = true;
    error = null;
    saved = null;
    try {
      const u = url.trim();
      const s = await putSettings({
        support: clear ? null : { name: name.trim(), email: email.trim(), url: u || null },
      });
      showStored(s);
      settings.saved(s);
      saved = clear ? 'Cleared ✓' : 'Saved ✓';
    } catch (e) {
      error = errorMessage(e);
    } finally {
      saving = false;
    }
  }
</script>

<h3>Support contact</h3>
<p class="muted">
  Who the Lab view's <em>Ask</em> button emails when an analysis needs attention. A pipeline can
  name its own contact in its <code>biffix.yaml</code>, which wins for its analyses.
</p>
<div class="form">
  <label>
    <span class="lbl">Name</span>
    <input
      type="text"
      bind:value={name}
      placeholder="Sam Okafor"
      autocomplete="off"
      oninput={edited}
    />
  </label>
  <label>
    <span class="lbl">Email</span>
    <input
      type="email"
      bind:value={email}
      placeholder="help@lab.example.org"
      autocomplete="off"
      oninput={edited}
    />
  </label>
  <label>
    <span class="lbl">Help page <span class="hint">(optional)</span></span>
    <input
      type="url"
      bind:value={url}
      placeholder="https://help.lab.example.org/biffix"
      autocomplete="off"
      oninput={edited}
    />
  </label>
  <div class="actions">
    <button onclick={() => save(false)} disabled={saving || !dirty || !name.trim() || !email.trim()}
      >{saving ? 'Saving…' : 'Set contact'}</button
    >
    {#if settings.stored?.support}<button
        class="ghost"
        type="button"
        disabled={saving}
        onclick={() => save(true)}>Clear</button
      >{/if}
    {#if saved}<span class="ok" role="status">{saved}</span>{/if}
    {#if error}<span class="error" role="alert">{error}</span>{/if}
  </div>
</div>
