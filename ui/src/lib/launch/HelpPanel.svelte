<script lang="ts">
  // A parameter's long help, opened from its ⓘ: the author's text (from the workflow repo, so
  // untrusted) rendered as sanitized Markdown, so a paragraph break, a list or a link reads as one;
  // only inline formatting, lists, code and links (no heading, table or image).
  import { renderHelpMarkdown } from '#lib/markdown/markdown.js';

  interface Props {
    /** The panel's id, which its ⓘ button's `aria-controls` names. */
    id: string;
    help: string;
  }
  let { id, help }: Props = $props();
  const html = $derived(renderHelpMarkdown(help));
</script>

<div class="help" {id} role="note">
  {@html html}
</div>

<style>
  .help {
    margin: 0.3rem 0 0;
    padding: 0.4rem 0.6rem;
    border-left: 2px solid var(--accent);
    background: var(--hover);
    color: var(--text);
    font-size: 0.8rem;
    max-width: 60ch;
  }
  .help :global(p) {
    margin: 0 0 0.4rem;
  }
  .help :global(p:last-child),
  .help :global(ul:last-child),
  .help :global(ol:last-child) {
    margin-bottom: 0;
  }
  .help :global(ul),
  .help :global(ol) {
    margin: 0 0 0.4rem;
    padding-left: 1.2rem;
  }
  .help :global(a) {
    color: var(--accent);
  }
  .help :global(code) {
    font-family: var(--mono);
    font-size: 0.76rem;
  }
</style>
