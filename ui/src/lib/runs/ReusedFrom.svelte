<script lang="ts">
  import { runTabHref } from '#lib/runs/runNav.js';

  // Beside an output a resumed run reused rather than wrote again (the engine skipped the task): a
  // link to the nearest run it resumed that registered it (`RunOutput.reused_from`), not
  // necessarily the one that wrote the bytes. Nothing for the run's own outputs.
  interface Props {
    from: string | null | undefined;
  }
  let { from }: Props = $props();
</script>

{#if from}
  <span class="reused-from" title="An output of an earlier run this one resumed, reused here"
    >Reused from <a href={runTabHref(from, 'overview')}>{from}</a></span
  >
{/if}

<style>
  .reused-from {
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  .reused-from a {
    color: var(--accent);
  }
</style>
