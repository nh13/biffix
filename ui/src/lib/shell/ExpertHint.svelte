<script lang="ts">
  // One quiet line on a Lab page for whoever needs more than it shows (a bioinformatician a link was
  // shared with): what the Expert view has, and a button to switch to it on the same page. Shown in
  // the Lab view until dismissed; the dismissal holds for every page (`expertHint`).
  import { view } from '$lib/state/userPrefs.svelte';
  import { dismissExpertHint, expertHint } from './expertHintState.svelte';
  import { switchViewTo } from './switchView';

  interface Props {
    /** What the Expert view has that this page does not. */
    what?: string;
  }
  let { what = 'Looking for logs and task details?' }: Props = $props();
</script>

{#if view.mode === 'lab' && !expertHint.dismissed}
  <aside class="hint" aria-label="Expert view" data-testid="expert-hint">
    <p>
      {what}
      <button type="button" class="link" onclick={() => switchViewTo('expert')}
        >Switch to Expert view</button
      >
      <span class="where">(also at the top right of every page)</span>
    </p>
    <button
      type="button"
      class="dismiss"
      aria-label="Don't show this again"
      title="Don't show this again"
      onclick={dismissExpertHint}>×</button
    >
  </aside>
{/if}

<style>
  .hint {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.6rem;
    margin: 1.5rem 0 0;
    padding: 0.5rem 0.75rem;
    border: 1px dashed var(--card-border);
    border-radius: 0.5rem;
    font-size: 0.8rem;
  }
  p {
    margin: 0;
  }
  .where {
    white-space: nowrap;
  }
  .dismiss {
    flex: none;
    padding: 0 0.35rem;
    border: 0;
    border-radius: 0.3rem;
    background: none;
    font-size: 1rem;
    line-height: 1.2;
    color: var(--text-muted);
    cursor: pointer;
  }
  .dismiss:hover {
    color: var(--text);
    background: var(--hover);
  }
  .link:focus-visible,
  .dismiss:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
