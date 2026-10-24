<script lang="ts">
  // The first-visit tour's popover: one stop at a time, anchored beside the
  // element it points at (found by its `data-tour` attribute), or centred when that is not on
  // show. A non-modal dialog: focus moves to it, Tab leaves it freely, Escape skips the tour, and
  // the page under it stays usable. It floats over the page, so nothing moves when it opens.
  import { onMount, tick } from 'svelte';
  import { finishTour } from '#lib/state/userPrefs.svelte.js';
  import { TOUR_ANCHOR, TOUR_STOPS, placePopover, type Placement, type TourAnchor } from './tour';

  let index = $state(0);
  let popover = $state<HTMLElement>();
  let placement = $state<Placement | null>(null);
  const stop = $derived(TOUR_STOPS[index]);
  const last = $derived(index === TOUR_STOPS.length - 1);
  /** What had focus before the tour, to hand it back after. */
  let before: Element | null = null;

  /** The stop's anchor: its first `data-tour` element that is on show (not hidden, not inert). */
  function anchorOf(names: readonly TourAnchor[]): HTMLElement | null {
    for (const name of names) {
      for (const el of document.querySelectorAll<HTMLElement>(`[data-tour="${name}"]`)) {
        const r = el.getBoundingClientRect();
        if (r.width === 0 || r.height === 0 || el.closest('[inert]')) continue;
        if (el.checkVisibility && !el.checkVisibility({ visibilityProperty: true })) continue;
        return el;
      }
    }
    return null;
  }

  let marked: HTMLElement | null = null;
  /** Point at `el`: ring it (an outline, so nothing moves), and drop the ring from the last one. */
  function mark(el: HTMLElement | null) {
    if (el === marked) return;
    marked?.removeAttribute('data-tour-active');
    el?.setAttribute('data-tour-active', '');
    marked = el;
  }

  /** Watches the marked anchor's size (the page around it is watched from `onMount`). */
  let anchorSize: ResizeObserver | null = null;

  function place() {
    if (!popover) return;
    const el = anchorOf(stop.anchors);
    if (el !== marked) {
      anchorSize?.disconnect();
      if (el) anchorSize?.observe(el);
    }
    mark(el);
    const p = popover.getBoundingClientRect();
    const next = placePopover(
      el?.getBoundingClientRect() ?? null,
      { width: p.width, height: p.height },
      { width: window.innerWidth, height: window.innerHeight },
    );
    // Unchanged: leave it be (no re-render, and no style change for the page's observer to see).
    if (
      placement?.top === next.top &&
      placement.left === next.left &&
      placement.centred === next.centred
    )
      return;
    placement = next;
  }

  /** One `place()` per frame, however many changes asked for it. */
  let frame = 0;
  function schedule() {
    if (frame) return;
    frame = requestAnimationFrame(() => {
      frame = 0;
      place();
    });
  }

  /** Show stop `i`: bring its anchor into view, place the popover beside it, and focus it. */
  async function show(i: number) {
    index = i;
    await tick();
    const el = anchorOf(stop.anchors);
    if (el) {
      const r = el.getBoundingClientRect();
      if (r.top < 0 || r.bottom > window.innerHeight) el.scrollIntoView({ block: 'center' });
    }
    place();
    // Placed (and so visible) first: a hidden element cannot take focus.
    await tick();
    popover?.focus({ preventScroll: true });
  }

  /** End the tour (Skip, Escape or Done) and hand focus back. */
  function end() {
    mark(null);
    const back =
      before instanceof HTMLElement && before.isConnected && before !== document.body
        ? before
        : anchorOf([TOUR_ANCHOR.start]);
    void finishTour();
    back?.focus();
  }

  function onkeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || e.defaultPrevented) return;
    // Put away under the modal Learn sheet (a hidden ancestor), the tour is not on show: Escape
    // there is the sheet's, and must not end the tour.
    if (!popover || popover.closest('[hidden]')) return;
    // Escape skips from the popover, or from nowhere in particular (focus on the page itself).
    const at = document.activeElement;
    if (popover.contains(at) || at === document.body || at === null) {
      e.preventDefault();
      end();
    }
  }

  onMount(() => {
    before = document.activeElement;
    // The page under it loads, scrolls, resizes and shows or hides things (an anchor mounting late,
    // the sidebar collapsing, the drawer opening): keep the popover beside its anchor. A size
    // change is seen by the observers; a change in what is on show, by the mutations of the page's
    // elements and their `class`, `hidden` and `inert`.
    anchorSize = new ResizeObserver(schedule);
    const pageSize = new ResizeObserver(schedule);
    pageSize.observe(document.body);
    for (const el of document.querySelectorAll('main, .app')) pageSize.observe(el);
    const shown = new MutationObserver(schedule);
    shown.observe(document.body, {
      subtree: true,
      childList: true,
      attributes: true,
      attributeFilter: ['class', 'hidden', 'inert'],
    });
    window.addEventListener('resize', schedule);
    window.addEventListener('scroll', schedule, true);
    void show(0);
    return () => {
      window.removeEventListener('resize', schedule);
      window.removeEventListener('scroll', schedule, true);
      anchorSize?.disconnect();
      pageSize.disconnect();
      shown.disconnect();
      cancelAnimationFrame(frame);
      mark(null);
    };
  });
</script>

<svelte:window {onkeydown} />

<div
  class="tour"
  class:centred={placement?.centred}
  bind:this={popover}
  role="dialog"
  aria-labelledby="tour-title"
  aria-describedby="tour-body"
  tabindex="-1"
  style:top={placement ? `${placement.top}px` : '0'}
  style:left={placement ? `${placement.left}px` : '0'}
  style:visibility={placement ? 'visible' : 'hidden'}
>
  <p class="step">Tour · {index + 1} of {TOUR_STOPS.length}</p>
  <h2 id="tour-title">{stop.title}</h2>
  <p id="tour-body">{stop.body}</p>
  <div class="acts">
    <button type="button" class="skip" onclick={end}>Skip tour</button>
    <span class="nav">
      {#if index > 0}
        <button type="button" onclick={() => show(index - 1)}>Back</button>
      {/if}
      {#if last}
        <button type="button" class="primary" onclick={end}>Done</button>
      {:else}
        <button type="button" class="primary" onclick={() => show(index + 1)}>Next</button>
      {/if}
    </span>
  </div>
</div>

<style>
  .tour {
    position: fixed;
    z-index: 60;
    width: min(20rem, calc(100vw - 2rem));
    box-sizing: border-box;
    padding: 0.8rem 0.95rem 0.75rem;
    border: 1px solid var(--accent);
    border-radius: 0.6rem;
    background: var(--bg-content);
    color: var(--text);
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.18);
  }
  .tour:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .step {
    margin: 0;
    font-size: 0.72rem;
    color: var(--text-muted);
  }
  h2 {
    margin: 0.15rem 0 0.3rem;
    font-size: 0.95rem;
  }
  #tour-body {
    margin: 0 0 0.7rem;
    font-size: 0.84rem;
    line-height: 1.45;
  }
  .acts {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
  }
  .nav {
    display: inline-flex;
    gap: 0.4rem;
  }
  button {
    padding: 0.3rem 0.75rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
    font-size: 0.82rem;
    cursor: pointer;
  }
  button:hover {
    background: var(--hover);
  }
  button.primary {
    border-color: var(--tab-active-bg);
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
  }
  .skip {
    border-color: transparent;
    padding-left: 0.2rem;
    padding-right: 0.2rem;
    color: var(--text-muted);
    text-decoration: underline;
  }
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  /* The element a stop points at. An outline, so nothing on the page moves. */
  :global([data-tour-active]) {
    outline: 2px solid var(--accent) !important;
    outline-offset: 3px;
  }
</style>
