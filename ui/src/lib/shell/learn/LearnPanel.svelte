<script lang="ts">
  // The Learn panel: the page's pitch, three how-to cards and a link to the user guide, in the
  // words of the view on show, following the page as the user moves
  // around. On a wide screen it is a side panel, not a modal, so the page stays in view and usable.
  // On a narrow one it covers the page, so it is a modal sheet: the caller makes the page under it
  // inert. Focus moves to it on open; Escape or Close closes it (the caller hands focus back to the
  // button that opened it).
  import { onMount } from 'svelte';
  import { renderMarkdown } from '#lib/markdown/markdown.js';
  import type { ViewMode } from '#lib/view/viewRoutes.js';
  import { GUIDE_URL, loadLearn, type LearnContent } from './learn';

  interface Props {
    path: string;
    mode: ViewMode;
    /** Whether it is a modal sheet over the page (a narrow screen), not a panel beside it. */
    modal: boolean;
    onclose: () => void;
  }
  let { path, mode, modal, onclose }: Props = $props();

  // The page's content, loaded when the panel opens and again as the page or view changes. The last
  // page's stays on show until the next has loaded, so nothing flashes empty in between.
  let content = $state<LearnContent | null>(null);
  // A content chunk that could not load (gone after a redeploy, or offline): said, not a blank.
  let loadError = $state(false);
  $effect(() => {
    let cancelled = false;
    loadLearn(path, mode)
      .then((c) => {
        if (cancelled) return;
        content = c;
        loadError = false;
      })
      .catch(() => {
        if (cancelled) return;
        content = null;
        loadError = true;
      });
    return () => {
      cancelled = true;
    };
  });
  let heading = $state<HTMLElement>();

  onMount(() => heading?.focus());

  function onkeydown(e: KeyboardEvent) {
    if (e.key === 'Escape' && !e.defaultPrevented) {
      e.preventDefault();
      onclose();
    }
  }
</script>

<!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
<div
  class="learn"
  class:modal
  id="learn-panel"
  role={modal ? 'dialog' : 'complementary'}
  aria-modal={modal ? 'true' : undefined}
  aria-labelledby="learn-title"
  {onkeydown}
>
  <div class="head">
    <h2 id="learn-title" tabindex="-1" bind:this={heading}>
      {content?.title ?? 'Learn'}
    </h2>
    <button type="button" class="close" onclick={onclose} aria-label="Close the Learn panel"
      >✕</button
    >
  </div>
  {#if content}
    <!-- Our own Markdown, still through the app's sanitizer. -->
    <div class="pitch">{@html renderMarkdown(content.pitch)}</div>
    {#each content.cards as card, i (i)}
      <section class="card">
        <h3>{card.title}</h3>
        {@html renderMarkdown(card.body)}
      </section>
    {/each}
    <a class="docs" href={GUIDE_URL} target="_blank" rel="noopener noreferrer"
      >Read the guide <span class="sr-only">(opens in a new tab)</span>→</a
    >
  {/if}
  {#if loadError}
    <p class="muted" role="status">Couldn't load the help for this page. Reload to try again.</p>
  {/if}
</div>

<style>
  .learn {
    position: fixed;
    top: 0;
    right: 0;
    bottom: 0;
    z-index: 45;
    width: var(--learn-width);
    box-sizing: border-box;
    overflow-y: auto;
    padding: 1rem 1.1rem 1.5rem;
    background: var(--bg-sidebar);
    border-left: 1px solid var(--border);
    color: var(--text);
    box-shadow: -4px 0 18px rgb(0 0 0 / 0.08);
  }
  /* A narrow screen's sheet covers the page (which is inert under it). */
  .learn.modal {
    width: 100%;
    border-left: none;
  }
  .head {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 0.5rem;
  }
  h2 {
    margin: 0;
    font-size: 1rem;
  }
  h2:focus {
    outline: none;
  }
  h2:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .close {
    flex-shrink: 0;
    width: 1.9rem;
    height: 1.9rem;
    border: 1px solid var(--control-border);
    border-radius: 999px;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
  }
  .close:hover {
    background: var(--hover);
  }
  .close:focus-visible,
  .docs:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .pitch {
    margin: 0.5rem 0 0.9rem;
    font-size: 0.85rem;
    line-height: 1.5;
    color: var(--text);
  }
  .pitch :global(p) {
    margin: 0;
  }
  .card {
    margin-bottom: 0.65rem;
    padding: 0.65rem 0.75rem;
    border-radius: 0.5rem;
    font-size: 0.82rem;
    line-height: 1.45;
  }
  h3 {
    margin: 0 0 0.25rem;
    font-size: 0.85rem;
  }
  .card :global(p) {
    margin: 0;
  }
  .card :global(code),
  .pitch :global(code) {
    font-family: var(--mono);
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }
  .docs {
    display: inline-block;
    margin-top: 0.4rem;
    color: var(--accent);
    font-size: 0.85rem;
    text-decoration: underline;
  }
</style>
