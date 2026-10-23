<script lang="ts">
  // The narrow screen's top bar: the menu button that opens the sidebar drawer, the site's name, and
  // the page tools (hidden on a wide screen, where the sidebar and the view bar carry them).
  import Icon from '$lib/components/Icon.svelte';
  import { TOUR_ANCHOR } from './tour';
  import Brand from './Brand.svelte';
  import PageTools from './PageTools.svelte';
  import type { LearnPanelState } from './learnPanel.svelte';

  interface Props {
    drawerOpen: boolean;
    onmenu: () => void;
    learn: LearnPanelState;
  }
  let { drawerOpen, onmenu, learn }: Props = $props();

  let menuButton = $state<HTMLButtonElement>();
  /** Hand focus back to the menu button (the drawer has just closed). */
  export function focusMenu() {
    menuButton?.focus();
  }
</script>

<header class="topbar">
  <button
    class="icon-btn"
    bind:this={menuButton}
    onclick={onmenu}
    aria-label="Menu"
    aria-expanded={drawerOpen}
    aria-controls="app-sidebar"
    data-tour={TOUR_ANCHOR.menu}
  >
    <Icon name="menu" />
  </button>
  <Brand />
  <span class="topbar-toggle"><PageTools {learn} /></span>
</header>

<style>
  .topbar {
    display: none;
  }
  .topbar-toggle {
    margin-left: auto;
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
  }
  @media (max-width: 800px) {
    .topbar {
      display: flex;
      align-items: center;
      gap: 0.5rem;
      position: sticky;
      top: 0;
      z-index: 30;
      padding: 0.4rem 0.75rem;
      background: var(--bg-sidebar);
      border-bottom: 1px solid var(--border);
    }
  }
</style>
