<script lang="ts">
  // Settings › Help: replay the first-visit tour and bring back the "Switch to Expert" hint. Saved
  // for the user, like the view: the tour's done state follows them to every browser.
  import { goto } from '$app/navigation';
  import { expertHint, restoreExpertHint } from '$lib/shell/expertHintState.svelte';
  import { replayTour, view } from '$lib/state/userPrefs.svelte';

  // Replay the first-visit tour: My work shows it again (in the Lab view, where it lives).
  async function replay() {
    void replayTour();
    await goto('/');
  }
</script>

<div class="settings-section">
  <h2>Help <span class="scope">your account</span></h2>
  {#if view.mode === 'lab'}
    <p class="muted">The tour of My work shows the four things to know first.</p>
    <div class="actions">
      <button class="ghost" type="button" onclick={replay}>Replay the tour</button>
    </div>
    <p class="muted">
      Analysis and pipeline pages say where the Expert view's logs and task details are, until you
      dismiss it.
    </p>
    <div class="actions">
      {#if expertHint.dismissed}
        <button class="ghost" type="button" onclick={restoreExpertHint}
          >Show the "Switch to Expert" hint again</button
        >
      {:else}
        <span class="muted">The hint is showing.</span>
      {/if}
    </div>
  {:else}
    <p class="muted">
      The first-visit tour is part of the Lab view: switch to Lab above to replay it on My work.
    </p>
  {/if}
</div>
