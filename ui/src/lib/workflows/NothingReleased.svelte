<script lang="ts">
  // What a Workflows page says when the Developer filter left out every pipeline: none has a
  // released version yet. Never an empty page without a reason, and the way to see them all.
  import { setShowDeveloper } from '#lib/state/localPrefs.svelte.js';

  let { count, noun = 'pipeline' }: { count: number; noun?: string } = $props();
</script>

<div class="nothing" role="status">
  <p>
    No {noun} has a released version yet, so {count === 1
      ? `the one ${noun} is`
      : `all ${count} ${noun}s are`} in development. A developer releases a version from its page.
  </p>
  <button type="button" onclick={() => setShowDeveloper(true)}
    >Show developer {noun}s ({count})</button
  >
</div>

<style>
  .nothing {
    padding: 0.9rem 1rem;
    border: 1px dashed var(--control-border);
    border-radius: 0.6rem;
    max-width: 40rem;
  }
  p {
    margin: 0 0 0.6rem;
    color: var(--text-muted);
  }
  button {
    font: inherit;
    font-size: 0.82rem;
    padding: 0.25rem 0.8rem;
    border-radius: 0.4rem;
    border: 1px solid var(--control-border);
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
