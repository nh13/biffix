<script lang="ts">
  // What sits above every page: the Learn "?", the bell and the Lab | Expert switch. Shown in the
  // narrow top bar and in the wide view bar (one is hidden at any width).
  import NotificationBell from '#lib/notifications/NotificationBell.svelte';
  import ViewToggle from './ViewToggle.svelte';
  import type { LearnPanelState } from './learnPanel.svelte';

  let { learn }: { learn: LearnPanelState } = $props();
</script>

<button
  type="button"
  class="learn-btn"
  aria-label="Learn about this page"
  title="Learn about this page"
  aria-expanded={learn.open}
  aria-controls="learn-panel"
  onclick={(e) => learn.toggle(e)}>?</button
><NotificationBell /><ViewToggle />

<style>
  .learn-btn {
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 1.75rem;
    height: 1.75rem;
    border: 1px solid var(--control-border);
    border-radius: 999px;
    background: var(--bg-content);
    color: var(--text-nav);
    font: inherit;
    font-weight: 600;
    cursor: pointer;
  }
  .learn-btn:hover {
    background: var(--hover);
  }
  .learn-btn[aria-expanded='true'] {
    background: var(--active);
    color: var(--text);
  }
  .learn-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
