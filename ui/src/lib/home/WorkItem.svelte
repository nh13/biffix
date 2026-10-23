<script lang="ts">
  // One item of a My work list (an analysis or an experiment): its name, a line under it, an
  // optional flag, an optional middle column (an analysis's status) and Open. The rows share this
  // shell, so their spacing, edge colour and Open button stay one design.
  import type { Snippet } from 'svelte';

  let {
    href,
    name,
    tone = '',
    sub,
    flag = '',
    middle,
    onDismiss,
  }: {
    href: string;
    name: string;
    /** The left edge's colour: a `StatusTone` (`running`, `done`, `attention`, …) or none. */
    tone?: string;
    /** The line under the name. */
    sub: Snippet;
    /** A short warning under that line (none when empty). */
    flag?: string;
    /** A column between the name and Open. */
    middle?: Snippet;
    /** Offer Dismiss beside Open (a Needs attention row): called on click; never navigates. */
    onDismiss?: () => void;
  } = $props();
</script>

<li class="row {tone}" class:with-middle={!!middle}>
  <div class="what">
    <a class="name" {href}>{name}</a>
    <span class="sub">{@render sub()}</span>
    {#if flag}<span class="flag">{flag}</span>{/if}
  </div>
  {@render middle?.()}
  <div class="actions">
    <a class="open" {href}>Open<span class="sr-only"> {name}</span></a>
    {#if onDismiss}
      <button type="button" class="dismiss" aria-label={`Dismiss ${name}`} onclick={onDismiss}
        >Dismiss</button
      >
    {/if}
  </div>
</li>

<style>
  .row {
    display: grid;
    grid-template-columns: minmax(0, 1fr) auto;
    align-items: center;
    gap: 1rem;
    padding: 0.7rem 0.9rem;
    border: 1px solid var(--card-border);
    border-left-width: 3px;
    border-radius: 0.5rem;
    list-style: none;
  }
  .with-middle {
    grid-template-columns: minmax(0, 1fr) minmax(0, 1fr) auto;
  }
  .running {
    border-left-color: var(--accent);
  }
  .done {
    border-left-color: var(--fg-ok);
  }
  .attention {
    border-left-color: var(--fg-fail);
  }
  .what {
    display: flex;
    flex-direction: column;
    min-width: 0;
  }
  .name {
    font-weight: 600;
    color: var(--text);
    text-decoration: none;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .name:hover {
    text-decoration: underline;
  }
  .sub {
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .flag {
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--fg-fail);
    white-space: nowrap;
  }
  .open {
    padding: 0.25rem 0.75rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    color: var(--text);
    text-decoration: none;
    font-size: 0.8rem;
  }
  .open:hover,
  .dismiss:hover {
    background: var(--hover);
  }
  .dismiss {
    padding: 0.25rem 0.75rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: none;
    color: var(--text-muted);
    font: inherit;
    font-size: 0.8rem;
    cursor: pointer;
  }
  .open:focus-visible,
  .dismiss:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  @media (max-width: 640px) {
    .with-middle {
      grid-template-columns: minmax(0, 1fr) auto;
    }
    .with-middle > :global(.status) {
      grid-column: 1 / -1;
      grid-row: 2;
    }
  }
</style>
