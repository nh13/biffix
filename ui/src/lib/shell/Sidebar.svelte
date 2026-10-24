<script lang="ts">
  // The sidebar: the site's name, the view's nav, and the theme, sign-out and identity footer. On a
  // wide screen it sits beside the page (collapsible); on a narrow one the app shell shows it as a
  // drawer over the page.
  import { page } from '$app/state';
  import { logout } from '#lib/api/auth.js';
  import Icon from '#lib/components/Icon.svelte';
  import { healthOf, liveStatus } from '#lib/status/live.svelte.js';
  import { healthColor as colorOf, healthWords } from './health';
  import type { Theme } from '#lib/state/localPrefs.svelte.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { TOUR_ANCHOR } from './tour';
  import { isCurrent, navFor } from './nav';
  import Brand from './Brand.svelte';

  interface Props {
    /** The signed-in user's name, shown at the foot. */
    identity: string | null;
    theme: Theme;
    ontoggletheme: () => void;
    oncollapse: () => void;
    /** Off-screen (a closed drawer): its links must not take keyboard focus. */
    inert: boolean;
  }
  let { identity, theme, ontoggletheme, oncollapse, inert }: Props = $props();

  const nav = $derived(navFor(view.mode));
  // Overall service health drives the Status nav-icon color (see ./health). It reflects EVERY probe
  // of the shared status poll (which the status page reads too), a failure included.
  const health = $derived(healthOf(liveStatus.report, liveStatus.error));
  const healthColor = $derived(colorOf(health));
  const healthText = $derived(healthWords(health));

  let sidebar = $state<HTMLElement>();
  /** Move focus to the first nav link (the drawer has just opened). */
  export function focusFirstLink() {
    sidebar?.querySelector<HTMLElement>('.nav a')?.focus();
  }
</script>

<aside class="sidebar" id="app-sidebar" bind:this={sidebar} {inert}>
  <div class="sidebar-top">
    <Brand />
    <button
      class="icon-btn"
      onclick={oncollapse}
      aria-label="Collapse sidebar"
      title="Collapse sidebar"
    >
      <Icon name="panel-close" />
    </button>
  </div>

  <nav class="nav">
    {#each nav as item}
      {@const current = isCurrent(item.href, page.url.pathname)}
      <a
        href={item.href}
        class="nav-item"
        class:active={current}
        aria-current={current ? 'page' : undefined}
        title={item.label}
        data-tour={item.href === '/workflows' ? TOUR_ANCHOR.navPipelines : undefined}
      >
        <span class="nav-icon">
          {#if item.health}
            <Icon name={item.icon} class="status-icon" style={`color:${healthColor}`} />
          {:else}
            <Icon name={item.icon} />
          {/if}
        </span>
        <span class="nav-label">{item.label}</span>
        {#if item.health && healthText}<span class="sr-only"> ({healthText})</span>{/if}
      </a>
    {/each}
  </nav>

  <div class="sidebar-footer">
    <button class="theme-toggle" onclick={ontoggletheme} aria-label="Toggle theme">
      <span class="nav-icon">
        <Icon name={theme === 'dark' ? 'sun' : 'moon'} />
      </span>
      <span class="nav-label">{theme === 'dark' ? 'Light' : 'Dark'}</span>
    </button>

    <button class="theme-toggle" onclick={() => logout()} aria-label="Sign out" title="Sign out">
      <span class="nav-icon">
        <Icon name="logout" />
      </span>
      <span class="nav-label">Sign out</span>
    </button>

    {#if identity}
      <span class="tag" title={identity}>{identity}</span>
    {/if}
  </div>
</aside>

<style>
  .sidebar {
    width: var(--sidebar-width);
    flex-shrink: 0;
    height: 100vh;
    position: sticky;
    top: 0;
    display: flex;
    flex-direction: column;
    padding: 0.6rem;
    background: var(--bg-sidebar);
    border-right: 1px solid var(--border);
    color: var(--text-nav);
  }
  :global(.app.collapsed) .sidebar {
    display: none;
  }

  .sidebar-top {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0.25rem 0.4rem 0.75rem;
  }

  .nav {
    display: flex;
    flex-direction: column;
    gap: 0.1rem;
  }
  .nav-item {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    padding: 0.45rem 0.5rem;
    border-radius: 0.5rem;
    text-decoration: none;
    color: var(--text-nav);
    font-size: 0.82rem;
    font-weight: 500;
    transition: background 0.1s;
  }
  .nav-item:hover {
    background: var(--hover);
  }
  .nav-item.active {
    background: var(--active);
    color: var(--text);
  }
  .nav-icon {
    display: inline-flex;
    flex-shrink: 0;
  }
  .nav-icon :global(svg) {
    width: 1rem;
    height: 1rem;
  }

  .sidebar-footer {
    margin-top: auto;
    display: flex;
    flex-direction: column;
    gap: 0.5rem;
    padding: 0.3rem;
  }
  .theme-toggle {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    border: none;
    background: transparent;
    color: var(--text-nav);
    border-radius: 0.5rem;
    padding: 0.45rem 0.5rem;
    font-size: 0.82rem;
    font-weight: 500;
    cursor: pointer;
    text-align: left;
  }
  .theme-toggle:hover {
    background: var(--hover);
  }
  .tag {
    font-size: 0.68rem;
    color: var(--text-muted);
    padding-left: 0.5rem;
  }

  /* Narrow screens: a drawer over the page, slid out of view until the menu opens it. */
  @media (max-width: 800px) {
    .sidebar,
    :global(.app.collapsed) .sidebar {
      display: flex;
      position: fixed;
      top: 0;
      left: 0;
      bottom: 0;
      z-index: 50;
      width: min(80vw, var(--sidebar-width));
      transform: translateX(-100%);
      transition: transform 0.15s ease-out;
    }
    :global(.app.drawer-open) .sidebar {
      transform: none;
      box-shadow: 0 0 24px rgb(0 0 0 / 0.25);
    }
    .sidebar-top .icon-btn {
      display: none;
    }
  }
  @media (prefers-reduced-motion: reduce) {
    .sidebar {
      transition: none;
    }
  }
</style>
