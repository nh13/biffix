<script lang="ts">
  // Settings › Engines: the engine releases a workflow may pin (read-only; set by the biffix
  // release).
  import type { EnginesState } from './settingsState.svelte';

  let { engines }: { engines: EnginesState } = $props();
</script>

<div class="settings-section">
  <h2>Engines</h2>
  <p class="muted">
    The engine releases a workflow may pin with <code>engine_version</code> in its
    <code>biffix.yaml</code>: one exact release in the range below.
  </p>
  {#if engines.error}
    <p class="error">{engines.error}</p>
  {:else if engines.engines}
    <table class="engines">
      <thead
        ><tr><th>Engine</th><th>From</th><th>Up to (not including)</th><th>Releases</th></tr></thead
      >
      <tbody>
        {#each engines.engines as e (e.engine)}
          <tr>
            <td>{e.engine}</td><td>{e.min_version}</td><td>{e.below_version}</td>
            <td>{e.releases?.length ? e.releases.join(', ') : 'any in range'}</td>
          </tr>
        {/each}
      </tbody>
    </table>
  {:else}
    <p class="muted">Loading…</p>
  {/if}
</div>

<style>
  .engines {
    max-width: 520px;
  }
</style>
