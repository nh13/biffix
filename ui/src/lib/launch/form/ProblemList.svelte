<script lang="ts">
  // What is left to fix, as the launch bar and the wizard's footer say it: a count, the first two
  // problems as links to them, and how many more.
  import { plural } from '#lib/util/format.js';
  import { problemTitle, type Problem } from '../form';
  import { launchForm } from './launchForm.svelte';

  let { list }: { list: Problem[] } = $props();
  const form = launchForm();
</script>

<span class="count">⚠ {plural(list.length, 'problem')}</span>
{#each list.slice(0, 2) as p}
  <button class="link" onclick={() => form.focusProblem(p)}
    >{problemTitle(p, form.problemLabel(p.field))}</button
  >
{/each}
{#if list.length > 2}<span class="muted">… and {list.length - 2} more</span>{/if}

<style>
  .count {
    color: var(--fg-warn);
    font-weight: 600;
  }
  .link {
    font-size: 0.82rem;
  }
  .link:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .muted {
    font-size: 0.85rem;
  }
</style>
