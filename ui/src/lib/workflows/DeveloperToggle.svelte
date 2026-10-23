<script lang="ts">
  // The Workflows pages' Developer filter: off, they list only pipelines with a released version;
  // on, every pipeline. Remembered in this browser.
  import { prefs, setShowDeveloper } from '$lib/state/localPrefs.svelte';
</script>

<label class="developer" title="Also list pipelines and versions still in development">
  <input
    type="checkbox"
    checked={prefs.showDeveloper}
    onchange={(e) => setShowDeveloper((e.currentTarget as HTMLInputElement).checked)}
  />
  Developer
</label>

<style>
  .developer {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.82rem;
    color: var(--text-muted);
    cursor: pointer;
    white-space: nowrap;
  }
</style>
