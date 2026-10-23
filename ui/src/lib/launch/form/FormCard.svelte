<script lang="ts">
  // One card of the launch form: a section, labelled by its heading (or by `label` when it has
  // none), with the author's description under the heading. The Expert form's headings are h2; the
  // wizard's, under its step heading, h3.
  import type { Snippet } from 'svelte';

  interface Props {
    /** The heading's id, which labels the section. */
    id?: string;
    title?: string;
    level?: 2 | 3;
    description?: string | null;
    /** The section's label when it has no heading. */
    label?: string;
    class?: string;
    children: Snippet;
  }
  let { id, title, level = 2, description = null, label, class: extra, children }: Props = $props();
</script>

<section class={['card', extra]} aria-labelledby={title ? id : undefined} aria-label={label}>
  {#if title}
    {#if level === 3}<h3 {id}>{title}</h3>{:else}<h2 {id}>{title}</h2>{/if}
  {/if}
  {#if description}<p class="sec-desc">{description}</p>{/if}
  {@render children()}
</section>

<style>
  /* ReviewStep.svelte copies this card and its .card h3 rule: .card's margin and headings are
     per-component, so a change here needs the same change there. */
  .card {
    margin-bottom: 0.9rem;
  }
  .card h2 {
    margin: 0;
    font-size: 1rem;
  }
  .card h3 {
    margin: 0;
    font-size: 0.98rem;
  }
  .sec-desc {
    margin: 0.2rem 0 0.2rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
</style>
