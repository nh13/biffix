<script lang="ts">
  // Settings › Site (Expert view): the header title and logo, and the support contact. Each form is
  // offered only once the stored settings have loaded, and saves on its own.
  import type { SiteSettingsState } from './settingsState.svelte';
  import SiteForm from './SiteForm.svelte';
  import SupportContactForm from './SupportContactForm.svelte';

  let { settings }: { settings: SiteSettingsState } = $props();
</script>

<div class="settings-section">
  <h2>Site</h2>
  <p class="muted">
    The header title and logo, shown to everyone. Requires developer access to save.
  </p>

  {#if settings.loading}
    <p class="muted">Loading…</p>
  {:else if settings.loadError || !settings.stored}
    <div class="form">
      <p class="error" role="alert">Couldn't load the site settings: {settings.loadError}</p>
      <button class="ghost" type="button" onclick={() => settings.load()}>Retry</button>
    </div>
  {:else}
    <SiteForm {settings} />
    <SupportContactForm {settings} />
  {/if}
</div>
