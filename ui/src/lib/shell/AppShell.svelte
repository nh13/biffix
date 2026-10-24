<script lang="ts">
  // The app chrome around every signed-in page: the sidebar (a drawer on a narrow screen, behind
  // the top bar's menu button), the view bar above the page, the Learn panel, the first-visit tour
  // and the upload tray.
  import { onMount, tick, type Snippet } from 'svelte';
  import { page } from '$app/state';
  import { afterNavigate } from '$app/navigation';
  import Icon from '#lib/components/Icon.svelte';
  import UploadTray from '#lib/upload/UploadTray.svelte';
  import {
    saveSidebarCollapsed,
    savedSidebarCollapsed,
    type Theme,
  } from '#lib/state/localPrefs.svelte.js';
  import { finishTour, view } from '#lib/state/userPrefs.svelte.js';
  import { isExpertOnly } from '#lib/view/viewRoutes.js';
  import ExpertBanner from './ExpertBanner.svelte';
  import Tour from './Tour.svelte';
  import { TOUR_ANCHOR, shouldShowTour } from './tour';
  import LearnPanel from './learn/LearnPanel.svelte';
  import { LearnPanelState } from './learnPanel.svelte';
  import PageTools from './PageTools.svelte';
  import Sidebar from './Sidebar.svelte';
  import TopBar from './TopBar.svelte';

  interface Props {
    /** The signed-in user's name, shown at the sidebar's foot. */
    identity: string | null;
    theme: Theme;
    ontoggletheme: () => void;
    children: Snippet;
  }
  let { identity, theme, ontoggletheme, children }: Props = $props();

  const expertPageInLab = $derived(view.mode === 'lab' && isExpertOnly(page.url.pathname));
  const showTour = $derived(shouldShowTour(page.url.pathname, view.mode, view.tourDone));
  // Leaving My work mid-tour, in the Lab view, counts as having seen it: it does not start over on
  // every return. (Switching to Expert only hides it; it shows again back in Lab.)
  let tourWasShowing = false;
  $effect(() => {
    const showing = showTour;
    if (tourWasShowing && !showing && view.mode === 'lab' && view.tourDone === false)
      void finishTour();
    tourWasShowing = showing;
  });

  // The Learn panel. On a narrow screen it covers the page, so it is modal: everything under it is
  // inert while it is open.
  const learn = new LearnPanelState();

  let collapsed = $state(false);
  function toggleSidebar() {
    collapsed = !collapsed;
    saveSidebarCollapsed(collapsed);
  }

  // Narrow screens (the top bar is showing): the sidebar is a drawer, closed until the menu opens
  // it. Closed, it is `inert`: its links are off-screen, so they must not take keyboard focus.
  // Opening it moves focus to its first link; closing it (Escape, the backdrop) hands focus back to
  // the menu button. Following one of its links closes it and leaves focus to the new page.
  const NARROW = '(max-width: 800px)';
  let narrow = $state(false);
  const learnModal = $derived(learn.open && narrow);
  let drawerOpen = $state(false);
  let topBar = $state<ReturnType<typeof TopBar>>();
  let sidebar = $state<ReturnType<typeof Sidebar>>();
  afterNavigate(() => (drawerOpen = false));
  async function openDrawer() {
    drawerOpen = true;
    await tick();
    sidebar?.focusFirstLink();
  }
  function closeDrawer() {
    drawerOpen = false;
    topBar?.focusMenu();
  }
  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !drawerOpen || e.defaultPrevented) return;
    // Handled: a page's own Escape (closing a job's pane) leaves it alone.
    e.preventDefault();
    closeDrawer();
  }

  onMount(() => {
    collapsed = savedSidebarCollapsed();
    const narrowQuery = window.matchMedia(NARROW);
    narrow = narrowQuery.matches;
    const onNarrow = (e: MediaQueryListEvent) => (narrow = e.matches);
    narrowQuery.addEventListener('change', onNarrow);
    return () => narrowQuery.removeEventListener('change', onNarrow);
  });
</script>

<svelte:window {onkeydown} />

<div
  class="app"
  class:collapsed
  class:drawer-open={drawerOpen}
  class:learn-open={learn.open}
  inert={learnModal}
>
  <TopBar
    bind:this={topBar}
    {drawerOpen}
    onmenu={() => (drawerOpen ? closeDrawer() : openDrawer())}
    {learn}
  />
  {#if drawerOpen}
    <!-- svelte-ignore a11y_click_events_have_key_events, a11y_no_static_element_interactions -->
    <div class="drawer-backdrop" onclick={closeDrawer}></div>
  {/if}
  <Sidebar
    bind:this={sidebar}
    {identity}
    {theme}
    {ontoggletheme}
    oncollapse={toggleSidebar}
    inert={narrow && !drawerOpen}
  />

  <!-- Under the open drawer's backdrop: out of the tab order, as the Learn sheet's page is. -->
  <main class="content" inert={narrow && drawerOpen}>
    {#if collapsed}
      <button
        class="icon-btn reopen"
        onclick={toggleSidebar}
        aria-label="Open sidebar"
        title="Open sidebar"
        data-tour={TOUR_ANCHOR.reopen}
      >
        <Icon name="panel-open" />
      </button>
    {/if}
    <div class="view-bar" class:with-banner={expertPageInLab}>
      {#if expertPageInLab}<ExpertBanner />{/if}
      <span class="bar-toggle"><PageTools {learn} /></span>
    </div>
    {@render children()}
  </main>
</div>
{#if learn.open}
  <LearnPanel
    path={page.url.pathname}
    mode={view.mode}
    modal={learnModal}
    onclose={() => learn.close()}
  />
{/if}
<!-- Under a modal Learn sheet: the tour's popover is put away (not ended), and the tray is inert. -->
{#if showTour}<div hidden={learnModal}><Tour /></div>{/if}
<div inert={learnModal}><UploadTray /></div>

<style>
  .app {
    display: flex;
    min-height: 100vh;
  }

  .content {
    flex: 1;
    min-width: 0;
    max-width: calc(100% - var(--sidebar-width));
    background: var(--bg-content);
    color: var(--text);
    padding: 1.5rem 1.75rem;
    overflow-x: auto;
    position: relative;
  }
  /* A scrolling box would stop the run page's pinned tab bar from sticking to the viewport, so run
     pages clip instead, and scroll their wide tables in their own wrappers. */
  .content:has(:global(.run-page)) {
    overflow-x: clip;
  }
  .app.collapsed .content {
    max-width: 100%;
  }
  .reopen {
    position: absolute;
    top: 1rem;
    left: 0.75rem;
  }
  .app.collapsed .content {
    padding-top: 3rem;
  }
  /* The bell and the Lab | Expert switch (and, on an Expert page in Lab view, the way back) above every page. */
  .view-bar {
    display: flex;
    align-items: center;
    justify-content: flex-end;
    gap: 1rem;
    margin: -0.75rem 0 0.5rem;
    min-height: 1.75rem;
  }
  /* On a wide screen the Learn panel sits beside the page, which it pushes aside (and the upload
     tray moves left of it), so nothing is hidden under it. On a narrow one it is a modal sheet over
     the page. */
  @media (min-width: 801px) {
    .app.learn-open .content {
      margin-right: var(--learn-width);
    }
  }
  .view-bar.with-banner {
    justify-content: space-between;
  }
  /* The bell, then the switch. */
  .bar-toggle {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
  }
  /* Narrow screens: a top bar with a menu button, and the sidebar as a drawer over the page. */
  @media (max-width: 800px) {
    .app {
      flex-direction: column;
    }
    .drawer-backdrop {
      position: fixed;
      inset: 0;
      z-index: 40;
      background: rgb(0 0 0 / 0.35);
    }
    .content,
    .app.collapsed .content {
      max-width: 100%;
      padding: 1rem;
    }
    .reopen {
      display: none;
    }
    /* The top bar carries the switch on narrow screens. */
    .bar-toggle {
      display: none;
    }
    .view-bar:not(.with-banner) {
      display: none;
    }
  }
</style>
