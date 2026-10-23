<script lang="ts">
  // The Lab | Expert switch shown on every page. Switching keeps the user on the same object:
  // Expert keeps the URL, Lab leaves an Expert-only page for its Lab equivalent.
  import { view, type ViewMode } from '$lib/state/userPrefs.svelte';
  import { switchViewTo } from './switchView';
  import { TOUR_ANCHOR } from './tour';

  const MODES: { mode: ViewMode; label: string; hint: string }[] = [
    { mode: 'lab', label: 'Lab', hint: 'Lab view: analyses, samples and results, in plain words' },
    {
      mode: 'expert',
      label: 'Expert',
      hint: 'Expert view: jobs, logs, task details, costs and AWS details',
    },
  ];

  async function pick(mode: ViewMode) {
    if (mode !== view.mode) await switchViewTo(mode);
  }
</script>

<!-- Labelled on screen ("View"), so a colleague sees what the two words switch between. -->
<div class="toggle" role="group" aria-label="View" data-tour={TOUR_ANCHOR.viewToggle}>
  <span class="lbl" aria-hidden="true">View</span>
  {#each MODES as m}
    <button
      type="button"
      class:on={view.mode === m.mode}
      aria-pressed={view.mode === m.mode}
      title={m.hint}
      onclick={() => pick(m.mode)}>{m.label}</button
    >
  {/each}
</div>

<style>
  .toggle {
    display: inline-flex;
    align-items: center;
    border: 1px solid var(--control-border);
    border-radius: 999px;
    padding: 2px;
    background: var(--bg-content);
  }
  .lbl {
    padding: 0 0.35rem 0 0.55rem;
    font-size: 0.7rem;
    color: var(--text-muted);
  }
  /* The phone's top bar has no room for it: the two words stand alone there. */
  @media (max-width: 800px) {
    .lbl {
      display: none;
    }
  }
  button {
    border: none;
    background: transparent;
    color: var(--text-muted);
    font: inherit;
    font-size: 0.78rem;
    padding: 0.2rem 0.75rem;
    border-radius: 999px;
    cursor: pointer;
  }
  button:hover {
    color: var(--text);
  }
  button.on {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
