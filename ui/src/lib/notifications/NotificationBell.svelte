<script lang="ts">
  // The notification bell, in both views next to the Lab | Expert switch: the
  // unread count (in the failure colour while an unread one stopped with a problem), and a dropdown
  // of the latest notifications. Opening one goes to its run, whose page marks it read; "Mark all
  // read" clears them. A disclosure (`Disclosure`): the button toggles the panel after it, Escape
  // (or a click or focus outside) closes it, and Escape hands focus back to the bell. Newly arrived
  // ones are announced to a screen reader.
  import { tick } from 'svelte';
  import { Disclosure } from '$lib/util/disclosure.svelte';
  import Icon from '$lib/components/Icon.svelte';
  import {
    markRead,
    notifications,
    openedFromBell,
    watchNotifications,
  } from '$lib/notifications/notifications.svelte';
  import {
    badgeText,
    bellLabel,
    emptyText,
    notificationText,
    unreadSummary,
  } from '$lib/notifications/notify';
  import { runTabHref } from '$lib/runs/runNav';
  import { view } from '$lib/state/userPrefs.svelte';

  const uid = $props.id();
  const panelId = `${uid}-notifications`;
  const disclosure = new Disclosure();
  let title = $state<HTMLElement | null>(null);

  $effect(() => watchNotifications());

  const list = $derived(notifications.list);
  const unread = $derived(list?.unread ?? 0);
  const unreadFailed = $derived(list?.unread_failed ?? 0);
  const summary = $derived(unreadSummary(unread, unreadFailed, view.mode));

  // The run's page sends the read (one request); the bell shows it read at once.
  function openNotification(runId: string, read: boolean) {
    disclosure.close();
    if (!read) openedFromBell(runId);
  }

  // "Mark all read" goes away once nothing is unread: keep focus in the panel (on its title), so
  // the keyboard is not dropped to the page and Escape still closes it.
  async function markAll() {
    const reading = markRead({ all: true });
    await tick();
    title?.focus();
    await reading;
  }
</script>

<svelte:window onpointerdown={disclosure.onWindowPointer} />

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div
  class="bell"
  bind:this={disclosure.root}
  onkeydown={disclosure.onkeydown}
  onfocusout={disclosure.onfocusout}
>
  <button
    type="button"
    class="bell-btn"
    bind:this={disclosure.button}
    aria-label={bellLabel(unread, unreadFailed, view.mode)}
    aria-expanded={disclosure.open}
    aria-controls={panelId}
    title="Notifications"
    onclick={() => disclosure.toggle()}
  >
    <Icon name="bell" />
    {#if unread > 0}<span class="badge" class:attention={unreadFailed > 0} aria-hidden="true"
        >{badgeText(unread)}</span
      >{/if}
  </button>
  <span class="sr-only" aria-live="polite">{notifications.announcement}</span>
  {#if disclosure.open}
    <div class="panel" id={panelId} role="region" aria-labelledby={`${panelId}-title`}>
      <div class="panel-head">
        <div>
          <p class="panel-title" id={`${panelId}-title`} tabindex="-1" bind:this={title}>
            Notifications
          </p>
          {#if summary}<p class="summary" class:attention={unreadFailed > 0}>{summary}</p>{/if}
        </div>
        {#if unread > 0}
          <button type="button" class="link" onclick={markAll}>Mark all read</button>
        {/if}
      </div>
      {#if notifications.error}
        <p class="error" role="alert">Couldn't refresh notifications: {notifications.error}</p>
      {/if}
      {#if notifications.readError}
        <p class="error" role="alert">
          Couldn't mark notifications read: {notifications.readError}
        </p>
      {/if}
      {#if !list}
        {#if !notifications.error}<p class="muted">Loading…</p>{/if}
      {:else if list.items.length === 0}
        <p class="muted">{emptyText(view.mode)}</p>
      {:else}
        <ul>
          {#each list.items as n (n.id)}
            {@const text = notificationText(n, view.mode)}
            <li class:unread={!n.read}>
              <a
                href={runTabHref(n.run_id, 'overview')}
                onclick={() => openNotification(n.run_id, n.read)}
              >
                {#if !n.read}<span class="dot" aria-hidden="true"></span><span class="sr-only"
                    >Unread:
                  </span>{/if}
                <span class="title" class:failed={text.failed}>{text.title}</span>
                <span class="meta">{[text.detail, text.ago].filter(Boolean).join(' · ')}</span>
              </a>
            </li>
          {/each}
        </ul>
      {/if}
    </div>
  {/if}
</div>

<style>
  .bell {
    position: relative;
    display: inline-flex;
  }
  .bell-btn {
    position: relative;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    width: 2rem;
    height: 2rem;
    border: 1px solid var(--control-border);
    border-radius: 999px;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .bell-btn :global(svg) {
    width: 1rem;
    height: 1rem;
  }
  .bell-btn:hover {
    background: var(--hover);
  }
  .bell-btn:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .badge {
    position: absolute;
    top: -0.35rem;
    right: -0.4rem;
    min-width: 1.05rem;
    height: 1.05rem;
    padding: 0 0.25rem;
    box-sizing: border-box;
    border-radius: 999px;
    background: var(--accent);
    color: var(--bg-content);
    font-size: 0.68rem;
    font-weight: 700;
    line-height: 1.05rem;
    text-align: center;
  }
  /* An unread one stopped with a problem: the badge takes the failure colour. (The bell's
     unread failures are not My work's Needs attention, which lasts until dismissed.) */
  .badge.attention {
    background: var(--fg-fail);
  }
  .panel {
    position: absolute;
    top: calc(100% + 0.4rem);
    right: 0;
    z-index: 60;
    width: 22rem;
    max-height: min(32rem, 75vh);
    overflow-y: auto;
    padding: 0.6rem 0;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
    background: var(--bg-content);
    box-shadow: 0 8px 24px rgb(0 0 0 / 0.18);
    text-align: left;
  }
  .panel-head {
    display: flex;
    align-items: baseline;
    justify-content: space-between;
    gap: 1rem;
    padding: 0 0.9rem 0.4rem;
  }
  .panel-title {
    margin: 0;
    font-weight: 600;
    font-size: 0.88rem;
    color: var(--text);
  }
  .panel-title:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .summary {
    margin: 0.1rem 0 0;
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .summary.attention {
    color: var(--fg-fail);
  }
  /* Not underlined until hovered: the panel's actions sit apart from its text. */
  .link {
    text-decoration: none;
    font-size: 0.8rem;
  }
  .link:hover {
    text-decoration: underline;
  }
  .link:focus-visible,
  a:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: -2px;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li + li {
    border-top: 1px solid var(--border);
  }
  a {
    position: relative;
    display: flex;
    flex-direction: column;
    gap: 0.15rem;
    padding: 0.55rem 0.9rem 0.55rem 1.6rem;
    color: var(--text);
    text-decoration: none;
  }
  a:hover {
    background: var(--hover);
    text-decoration: none;
  }
  .dot {
    position: absolute;
    left: 0.7rem;
    top: 0.95rem;
    width: 0.45rem;
    height: 0.45rem;
    border-radius: 50%;
    background: var(--accent);
  }
  .title {
    font-size: 0.85rem;
    font-weight: 600;
    overflow-wrap: anywhere;
  }
  .title.failed {
    color: var(--fg-fail);
  }
  li:not(.unread) .title {
    font-weight: 500;
  }
  .meta {
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .muted,
  .error {
    margin: 0.3rem 0.9rem;
    font-size: 0.82rem;
  }
  /* Narrow screens: the bell sits mid-bar, so the panel spans the screen under the top bar. */
  @media (max-width: 800px) {
    .panel {
      position: fixed;
      top: 3rem;
      left: 1rem;
      right: 1rem;
      width: auto;
    }
  }
</style>
