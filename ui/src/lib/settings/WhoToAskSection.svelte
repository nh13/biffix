<script lang="ts">
  // Settings › Who to ask (Lab view): the site's support contact, read-only.
  import type { SiteSettingsState } from './settingsState.svelte';

  let { settings }: { settings: SiteSettingsState } = $props();
  const support = $derived(settings.stored?.support);
</script>

<div class="settings-section">
  <h2>Who to ask</h2>
  {#if support}
    <p class="muted" data-testid="settings-support">
      When an analysis needs attention, <em>Ask</em> goes to
      <strong>{support.name}</strong>
      (<a href={`mailto:${support.email}`}>{support.email}</a>){#if support.url}{' · '}<a
          href={support.url}
          target="_blank"
          rel="noopener">help page ↗</a
        >{/if}, unless its pipeline names its own contact.
    </p>
  {:else if settings.loadError}
    <!-- A failed read is not "no one is named". -->
    <p class="error" role="alert">Couldn't load who to ask: {settings.loadError}</p>
    <button class="ghost" type="button" onclick={() => settings.load()}>Retry</button>
  {:else if !settings.loading}
    <p class="muted" data-testid="settings-support">
      No one is named yet: ask whoever runs biffix for your lab. A pipeline can name its own
      contact.
    </p>
  {/if}
</div>
