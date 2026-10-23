<script lang="ts">
  // Whether a version is released (a Developer or Admin promoted it) or a developer version.
  import { releaseLabel } from './release';

  let { released }: { released: boolean | null | undefined } = $props();
  const label = $derived(releaseLabel({ release: { released } }));
</script>

<span
  class="release"
  class:released={label === 'Released'}
  data-testid="release-badge"
  title={label === 'Released'
    ? 'Released: a developer released this version for everyday use'
    : 'Developer: a version still in development, not released'}>{label}</span
>

<style>
  .release {
    display: inline-block;
    vertical-align: middle;
    font-size: 0.7rem;
    font-weight: 600;
    line-height: 1.4;
    color: var(--text-muted);
    border: 1px dashed var(--control-border);
    border-radius: 999px;
    padding: 0 0.45rem;
    white-space: nowrap;
  }
  .release.released {
    color: var(--fg-ok);
    border: 1px solid color-mix(in oklch, var(--fg-ok) 45%, transparent);
  }
</style>
