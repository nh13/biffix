<script lang="ts">
  // *Start from example*: the version's examples, each replacing the whole form (after asking, when
  // something was entered or pre-filled). The wizard's sits at the right of its step heading, so its
  // menu opens to the left (`align`).
  import { launchForm } from './launchForm.svelte';

  interface Props {
    align?: 'left' | 'right';
  }
  let { align = 'left' }: Props = $props();
  const form = launchForm();
  let wrap: HTMLElement;

  /** A menu's keys: the arrows (and Home / End) move between the examples, Escape closes. */
  function menuKeydown(e: KeyboardEvent) {
    if (e.key === 'Escape') {
      e.stopPropagation();
      form.exampleChoice.setOpen(false);
      wrap.querySelector<HTMLElement>('[aria-haspopup]')?.focus();
      return;
    }
    const items = [...wrap.querySelectorAll<HTMLElement>('[role="menuitem"]')];
    const at = items.indexOf(document.activeElement as HTMLElement);
    const to =
      e.key === 'ArrowDown'
        ? (at + 1) % items.length
        : e.key === 'ArrowUp'
          ? (at - 1 + items.length) % items.length
          : e.key === 'Home'
            ? 0
            : e.key === 'End'
              ? items.length - 1
              : null;
    if (to === null) return;
    e.preventDefault();
    items[to]?.focus();
  }
</script>

<svelte:window
  onpointerdown={(e) => {
    if (form.exampleChoice.open && !wrap.contains(e.target as Node))
      form.exampleChoice.setOpen(false);
  }}
/>

<!-- svelte-ignore a11y_no_static_element_interactions -->
<div class="menu-wrap" bind:this={wrap} onkeydown={menuKeydown}>
  <button
    class="btn btn-compact"
    aria-haspopup="menu"
    aria-expanded={form.exampleChoice.open}
    onclick={() => form.exampleChoice.setOpen(!form.exampleChoice.open)}
    >Start from example ▾</button
  >
  {#if form.exampleChoice.open}
    <ul class="menu" class:right={align === 'right'} role="menu">
      {#each form.examples as ex, i}
        <li role="none">
          <button role="menuitem" onclick={() => form.exampleChoice.choose(i)}>
            <span class="ex-name">{ex.name}</span>
            {#if ex.description}<span class="ex-desc">{ex.description}</span>{/if}
          </button>
          {#if form.exampleChoice.pending === i}
            <div class="confirm">
              Replace everything you've entered?
              <button class="btn btn-small btn-danger" onclick={() => form.exampleChoice.choose(i)}
                >Replace</button
              >
              <button class="btn btn-small" onclick={() => (form.exampleChoice.pending = null)}
                >Cancel</button
              >
            </div>
          {/if}
        </li>
      {/each}
    </ul>
  {/if}
</div>

<style>
  .menu-wrap {
    position: relative;
  }
  .menu {
    position: absolute;
    z-index: 5;
    top: calc(100% + 0.25rem);
    left: 0;
    min-width: 20rem;
    margin: 0;
    padding: 0.3rem;
    list-style: none;
    background: var(--bg-content);
    border: 1px solid var(--control-border);
    border-radius: 0.5rem;
    box-shadow: 0 6px 20px rgb(0 0 0 / 0.12);
  }
  .menu.right {
    left: auto;
    right: 0;
  }
  .menu button[role='menuitem'] {
    display: flex;
    flex-direction: column;
    align-items: flex-start;
    width: 100%;
    padding: 0.45rem 0.6rem;
    border: none;
    border-radius: 0.35rem;
    background: none;
    color: var(--text);
    cursor: pointer;
    text-align: left;
    font: inherit;
  }
  .menu button[role='menuitem']:hover {
    background: var(--hover);
  }
  .ex-name {
    font-size: 0.85rem;
    font-weight: 600;
  }
  .ex-desc {
    font-size: 0.76rem;
    color: var(--text-muted);
  }
  .confirm {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    padding: 0.3rem 0.6rem 0.5rem;
    font-size: 0.8rem;
  }
  @media (max-width: 800px) {
    .menu {
      min-width: min(20rem, 90vw);
    }
  }
</style>
