<script lang="ts">
  // Needs attention's Dismiss cell, in either view's runs table.
  let { name, ondismiss }: { name: string; ondismiss: () => void } = $props();
</script>

<td
  ><button type="button" class="dismiss" aria-label={`Dismiss ${name}`} onclick={ondismiss}
    >Dismiss</button
  ></td
>

<style>
  .dismiss {
    padding: 0.2rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text-muted);
    font-size: 0.78rem;
    cursor: pointer;
    white-space: nowrap;
  }
  .dismiss:hover {
    background: var(--hover);
  }
  .dismiss:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
