<script lang="ts">
  import '../app.css';
  import { onMount, onDestroy } from 'svelte';
  import { page } from '$app/state';
  import { currentIdentity, ensureAuthenticated } from '$lib/api/auth';
  import { DEFAULT_SITE_TITLE, loadSite, site } from '$lib/state/site.svelte';
  import { watchStatus } from '$lib/status/live.svelte';
  import { errorMessage } from '$lib/util/format';
  import { appearance, setThemeChoice, type Theme } from '$lib/state/localPrefs.svelte';
  import { uploads } from '$lib/upload/store';
  import { loadView } from '$lib/state/userPrefs.svelte';
  import AppShell from '$lib/shell/AppShell.svelte';

  let { children } = $props();

  let theme = $state<Theme>('light');
  let authReady = $state(false);
  let authError = $state<string | null>(null);
  let identity = $state<string | null>(null);
  // The site chrome, shared with the Settings page so a save shows here at once.
  const siteTitle = $derived(site.title ?? DEFAULT_SITE_TITLE);
  const siteLogo = $derived(site.logo);

  // Routes that must NOT trigger the auth-on-mount redirect: the callback is mid-handshake, and
  // /signed-out is where the user deliberately landed — redirecting from there would defeat
  // sign-out entirely. They also render without the app chrome.
  const CHROMELESS = ['/auth/callback', '/signed-out'];
  const chromeless = $derived(CHROMELESS.includes(page.url.pathname));

  // The shared status poll (the Status nav icon's tint, and the status page).
  let stopHealthPoll: (() => void) | undefined;

  function applyTheme(t: Theme) {
    theme = t;
    document.documentElement.classList.toggle('dark', t === 'dark');
  }
  // Only a choice (the toggle, or Settings) is recorded: until then, and after Settings' "Match my
  // computer", the theme follows the OS, as app.html's pre-paint script does, even when the OS
  // setting changes while the page is open.
  let osDark = $state(false);
  let themeReady = $state(false);
  $effect(() => {
    if (!themeReady) return;
    const choice = appearance.theme;
    applyTheme(choice === 'system' ? (osDark ? 'dark' : 'light') : choice);
  });
  function toggleTheme() {
    setThemeChoice(theme === 'dark' ? 'light' : 'dark');
  }

  // Theme only — DOM-side, safe on every route including the chromeless ones. onMount fires once
  // for the whole SPA, so auth/chrome setup below lives in $effects instead (see the comment there).
  onMount(() => {
    const dark = window.matchMedia('(prefers-color-scheme: dark)');
    osDark = dark.matches;
    themeReady = true;
    const followOs = (e: MediaQueryListEvent) => (osDark = e.matches);
    dark.addEventListener('change', followOs);
    return () => dark.removeEventListener('change', followOs);
  });

  // Authenticate once the route is known. This runs from an $effect, NOT onMount, because the OAuth
  // callback lands on /auth/callback (chromeless, so auth is skipped) and then client-navigates into
  // the app — onMount does not fire again for that SPA navigation, so an onMount-only gate would
  // leave the app unauthenticated-looking until a manual reload. The chromeless routes set authReady
  // directly (the callback is mid-handshake; /signed-out is deliberate). ensureAuthenticated may
  // redirect to the IdP (unloading the page); a config-fetch failure is surfaced, not swallowed into
  // a permanently blank page.
  $effect(() => {
    if (chromeless) {
      authReady = true;
      return;
    }
    // Already resolved for this session. A chromeless route's authReady is not a sign-in, so their
    // links into the app are full page loads (the callback's "Try again"; /signed-out goes via the
    // IdP), which start this check afresh.
    if (authReady || authError) return;
    let active = true;
    void (async () => {
      try {
        const ok = await ensureAuthenticated();
        if (active && ok) authReady = true;
      } catch (e) {
        if (active) authError = errorMessage(e);
      }
    })();
    return () => {
      active = false;
    };
  });

  // Initialize the app chrome (identity label, health poll, custom site title/logo) once authed and
  // showing chrome. Keyed off (authReady && !chromeless) rather than onMount so it also runs after
  // the post-callback navigation, when chromeless flips false with authReady already true.
  let chromeInitDone = false;
  $effect(() => {
    if (!authReady || chromeless || chromeInitDone) return;
    chromeInitDone = true;
    identity = currentIdentity();
    stopHealthPoll = watchStatus();
    void loadSite();
    void loadView();
    // Uploads a reload interrupted: the tray offers to resume them.
    uploads.restore();
  });

  // The tab title and favicon follow the site settings, a removed logo restoring the default icon.
  let defaultIcon: { href: string; type: string } | null = null;
  $effect(() => {
    if (!authReady || chromeless) return;
    document.title = site.pageTitle ? `${site.pageTitle} · ${siteTitle}` : siteTitle;
    let link = document.querySelector<HTMLLinkElement>('link[rel="icon"]');
    if (!link) {
      link = document.createElement('link');
      link.rel = 'icon';
      document.head.appendChild(link);
    }
    defaultIcon ??= { href: link.getAttribute('href') ?? '', type: link.type };
    if (siteLogo) {
      link.href = siteLogo;
      link.removeAttribute('type'); // the default's svg type would not match a raster logo
    } else {
      link.href = defaultIcon.href;
      if (defaultIcon.type) link.type = defaultIcon.type;
    }
  });

  onDestroy(() => stopHealthPoll?.());
</script>

{#if authReady && chromeless}
  {@render children()}
{:else if authReady}
  <AppShell {identity} {theme} ontoggletheme={toggleTheme}>{@render children()}</AppShell>
{:else if authError}
  <div class="startup-error" role="alert">
    <h1>Can't reach biffix</h1>
    <p>Sign-in couldn't start: {authError}</p>
    <button onclick={() => location.reload()}>Retry</button>
  </div>
{/if}

<style>
  .startup-error {
    max-width: 32rem;
    margin: 18vh auto;
    text-align: center;
  }
  .startup-error h1 {
    font-size: 1.15rem;
  }
  .startup-error p {
    color: var(--text-muted);
    font-size: 0.85rem;
  }
  .startup-error button {
    margin-top: 0.5rem;
    padding: 0.4rem 1rem;
    border: 1px solid var(--border);
    border-radius: 0.5rem;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  :root {
    --sidebar-width: 220px;
    --learn-width: 22rem;
    --mono: ui-monospace, 'SF Mono', 'Cascadia Code', Consolas, monospace;
    --gray-50: oklch(0.98 0 0);
    --gray-100: oklch(0.94 0 0);
    --gray-200: oklch(0.92 0 0);
    --gray-300: oklch(0.85 0 0);
    --gray-400: oklch(0.77 0 0);
    --gray-500: oklch(0.69 0 0);
    --gray-700: oklch(0.42 0 0);
    --gray-800: oklch(0.32 0 0);
    --gray-850: oklch(0.27 0 0);
    --gray-900: oklch(0.2 0 0);
    --gray-950: oklch(0.16 0 0);

    --bg-content: #fff;
    --bg-sidebar: var(--gray-50);
    --border: var(--gray-100);
    --text: var(--gray-900);
    /* Muted and colored text all meet WCAG AA contrast (4.5:1) on the content background. */
    --text-muted: oklch(0.52 0 0);
    --accent: #2f5bd0;
    --fg-ok: #1a7f37;
    --fg-warn: #8a5a00;
    --fg-fail: #b42318;
    --fg-engine: #5b45c9;
    --text-nav: var(--gray-700);
    --hover: var(--gray-100);
    --active: var(--gray-200);
    --th-bg: var(--gray-50);
    --tab-bg: var(--gray-100);
    --tab-hover: var(--gray-200);
    --tab-active-bg: var(--gray-900);
    --tab-active-fg: #fff;
    /* Form controls and cards: visible edges that stay quiet in either theme. */
    --control-border: var(--gray-300);
    --card-border: var(--gray-200);
    /* The checker tile an image preview sits on. */
    --tile-a: var(--gray-100);
    --tile-b: var(--gray-200);
    /* Native controls and scrollbars follow the theme. */
    color-scheme: light;
  }

  :global(html.dark) {
    --bg-content: var(--gray-900);
    --bg-sidebar: var(--gray-950);
    --border: var(--gray-850);
    --text: var(--gray-100);
    --text-muted: var(--gray-500);
    --accent: #8fb0ff;
    --fg-ok: #7bd69a;
    --fg-warn: #e6cf7a;
    --fg-fail: #f2a5a0;
    --fg-engine: #b8a9ff;
    --text-nav: var(--gray-300);
    --hover: var(--gray-800);
    --active: var(--gray-850);
    --th-bg: var(--gray-850);
    --tab-bg: var(--gray-850);
    --tab-hover: var(--gray-800);
    --tab-active-bg: var(--gray-100);
    --tab-active-fg: var(--gray-900);
    --control-border: var(--gray-700);
    --card-border: var(--gray-800);
    --tile-a: var(--gray-800);
    --tile-b: var(--gray-850);
    color-scheme: dark;
  }

  :global(html, body) {
    margin: 0;
    height: 100%;
  }
  :global(body) {
    /* OWUI's real base stack: system font first, Inter only as a fallback. */
    font-family:
      -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, Inter, ui-sans-serif, sans-serif;
    font-size: 0.875rem; /* 14px, matching OWUI's text-sm base */
    color: var(--text);
    background: var(--bg-content);
    -webkit-font-smoothing: antialiased;
  }

  /* No page-width cap: content fills the viewport so wide tables (runs, workflows) can use a
     widescreen fully and keep each row on one line. Pages that want a narrow column (settings form,
     auth pages) set their own max-width locally. */
</style>
