<script lang="ts">
  // The site's name (and logo, when one is set) at the top of the sidebar and the narrow top bar.
  // The site settings are shared with the Settings page, so a save shows here at once.
  import { DEFAULT_SITE_TITLE, site } from '$lib/state/site.svelte';
</script>

<span class="brand">
  {#if site.logo}<img class="brand-logo" src={site.logo} alt="" />{/if}
  {site.title ?? DEFAULT_SITE_TITLE}
</span>

<style>
  .brand {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    font-size: 0.95rem;
    font-weight: 600;
    color: var(--text);
  }
  .brand-logo {
    max-height: 20px;
    max-width: 20px;
    object-fit: contain;
  }
</style>
