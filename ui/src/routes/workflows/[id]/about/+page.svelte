<script lang="ts">
  // The version's About page: the Markdown its author named with `about:` in biffix.yaml, rendered
  // safely (see $lib/markdown).
  import { renderMarkdown } from '$lib/markdown/markdown';
  import { getWorkflowContext } from '$lib/workflows/workflowContext.svelte';
  import { workflowTabHref } from '$lib/workflows/workflowNav';

  const wf = getWorkflowContext();
  const html = $derived(
    wf.detail?.about_markdown ? renderMarkdown(wf.detail.about_markdown) : null,
  );
</script>

{#if wf.detail}
  {#if html}
    <article class="about">
      <!-- Sanitized by renderMarkdown. -->
      {@html html}
    </article>
  {:else}
    <p class="muted">
      This version has no About page. <a href={workflowTabHref(wf.detail.id, 'run')}>Run it</a>.
    </p>
  {/if}
{/if}

<style>
  .about {
    max-width: 48rem;
    line-height: 1.55;
    overflow-wrap: anywhere;
  }
  .about :global(h1),
  .about :global(h2),
  .about :global(h3),
  .about :global(h4) {
    line-height: 1.25;
    margin: 1.4em 0 0.5em;
  }
  .about :global(h1) {
    font-size: 1.5rem;
  }
  .about :global(h2) {
    font-size: 1.2rem;
    padding-bottom: 0.25rem;
    border-bottom: 1px solid var(--border);
  }
  .about :global(h3) {
    font-size: 1.05rem;
  }
  .about :global(:first-child) {
    margin-top: 0;
  }
  .about :global(a) {
    color: var(--accent);
  }
  .about :global(code) {
    font-family: var(--mono);
    font-size: 0.85em;
    padding: 0.1em 0.3em;
    border-radius: 0.25rem;
    background: var(--hover);
  }
  .about :global(pre) {
    padding: 0.75rem 0.9rem;
    border: 1px solid var(--card-border);
    border-radius: 0.45rem;
    background: var(--hover);
    overflow-x: auto;
  }
  .about :global(pre code) {
    padding: 0;
    background: none;
  }
  .about :global(table) {
    border-collapse: collapse;
    display: block;
    width: fit-content;
    max-width: 100%;
    overflow-x: auto;
    font-size: 0.88rem;
  }
  .about :global(th),
  .about :global(td) {
    padding: 0.35rem 0.6rem;
    border: 1px solid var(--border);
    text-align: left;
  }
  .about :global(th) {
    background: var(--th-bg);
  }
  .about :global(blockquote) {
    margin: 0;
    padding-left: 0.9rem;
    border-left: 3px solid var(--border);
    color: var(--text-muted);
  }
  .about :global(img) {
    max-width: 100%;
  }
</style>
