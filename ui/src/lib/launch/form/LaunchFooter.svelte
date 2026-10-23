<script lang="ts">
  // The Expert form's sticky footer: what is left to fix (or that it is ready), a launch that
  // failed, the cost estimate, and the launch button.
  import ProblemList from './ProblemList.svelte';
  import { launchForm } from './launchForm.svelte';

  const form = launchForm();
</script>

<footer class="launch-bar">
  <div class="problems" aria-live="polite">
    {#if form.problems.length === 0}
      <span class="ready">✓ Ready to launch</span>
    {:else}
      <ProblemList list={form.problems} />
    {/if}
    {#if form.launchError}<span class="launch-error" role="alert">{form.launchError}</span>{/if}
    {#if form.estimate.text}<span
        class="estimate"
        title="From this workflow's recent successful runs">{form.estimate.text}</span
      >{/if}
  </div>
  <button
    class="btn btn-compact btn-primary launch"
    onclick={form.launch}
    disabled={form.launchDisabled}
    title={form.preview ? 'A preview cannot launch; register the workflow first' : undefined}
  >
    {form.launching ? 'Launching…' : form.from?.resume ? 'Resume run →' : 'Launch run →'}
  </button>
</footer>

<style>
  .estimate {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .launch-bar {
    position: sticky;
    bottom: 0;
    z-index: 4;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.75rem;
    padding: 0.7rem 1rem;
    margin: 0 -0.25rem;
    background: var(--bg-content);
    border-top: 1px solid var(--control-border);
  }
  .problems {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    flex-wrap: wrap;
    font-size: 0.82rem;
  }
  .ready {
    color: var(--fg-ok);
    font-weight: 600;
  }
  .launch-error {
    color: var(--fg-fail);
    font-size: 0.8rem;
  }
  .launch {
    font-size: 0.9rem;
    padding: 0.45rem 1rem;
  }
  @media (max-width: 800px) {
    .launch-bar {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
