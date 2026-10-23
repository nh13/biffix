<script lang="ts">
  // ‹ Previous · Next › on the analysis page: within the Analyses list the user came from
  // (its filter and order, as it last showed), else the user's own analyses newest first. `[` and
  // `]` do the same from the keyboard (not while typing in a field, nor behind an open dialog,
  // drawer or tour). The walk covers the page of the list the user saw: it ends at that page's
  // edge, which the position says.
  import { goto } from '$app/navigation';
  import { runTabHref } from '$lib/runs/runNav';
  import { listRuns } from '$lib/api';
  import { loadNavList, navKey, neighbours, overlayOpen, type NavItem } from './analysisNav';
  import { runName } from '$lib/runs/runName';

  interface Props {
    runId: string;
    /** Where "← Analyses" goes: the list the user came from, when known. */
    backHref?: string;
  }
  let { runId, backHref = $bindable('/runs') }: Props = $props();

  let items = $state<NavItem[]>([]);
  // The list the user came from has more pages than the one walked.
  let more = $state(false);
  // How many the user's own analyses ran to (the fallback list is their newest 100).
  const FALLBACK_LIMIT = 100;

  $effect(() => {
    const id = runId;
    const remembered = loadNavList();
    if (remembered && remembered.items.some((i) => i.id === id)) {
      items = remembered.items;
      backHref = remembered.href;
      more = remembered.more === true;
      return;
    }
    backHref = '/runs';
    items = [];
    more = false;
    let cancelled = false;
    listRuns('all', { mine: true, limit: FALLBACK_LIMIT })
      .then((page) => {
        if (cancelled) return;
        items = page.runs.map((r) => ({ id: r.run_id, name: runName(r) }));
        more = page.total > page.runs.length;
      })
      .catch(() => {
        /* no previous / next: the page reads the same without them */
      });
    return () => {
      cancelled = true;
    };
  });

  const near = $derived(neighbours(items, runId));
  const href = (i: NavItem) => runTabHref(i.id, 'overview');

  function onkeydown(e: KeyboardEvent) {
    const which = navKey(e, overlayOpen());
    const to = which === 'prev' ? near.prev : which === 'next' ? near.next : null;
    if (!to) return;
    e.preventDefault();
    void goto(href(to));
  }
</script>

<svelte:window {onkeydown} />

{#if near.index >= 0 && items.length > 1}
  <nav class="analysis-nav" aria-label="Previous and next analysis" data-testid="analysis-nav">
    {#if near.prev}
      <a
        href={href(near.prev)}
        rel="prev"
        aria-keyshortcuts="["
        aria-label={`Previous analysis: ${near.prev.name}`}
        title={`${near.prev.name} ([)`}>‹ Previous</a
      >
    {:else}
      <span class="off" aria-hidden="true">‹ Previous</span>
    {/if}
    <span class="pos"
      >{near.index + 1} of {items.length}{#if more}<span class="edge">
          on this page</span
        >{/if}</span
    >
    {#if near.next}
      <a
        href={href(near.next)}
        rel="next"
        aria-keyshortcuts="]"
        aria-label={`Next analysis: ${near.next.name}`}
        title={`${near.next.name} (])`}>Next ›</a
      >
    {:else}
      <span class="off" aria-hidden="true">Next ›</span>
    {/if}
  </nav>
{/if}

<style>
  .analysis-nav {
    display: inline-flex;
    align-items: center;
    gap: 0.6rem;
    font-size: 0.8rem;
  }
  a {
    color: var(--accent);
    text-decoration: none;
  }
  a:hover {
    text-decoration: underline;
  }
  a:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .off {
    color: var(--text-muted);
  }
  .pos {
    color: var(--text-muted);
  }
</style>
