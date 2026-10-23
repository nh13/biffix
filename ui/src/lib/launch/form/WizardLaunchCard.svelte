<script lang="ts">
  // The wizard's Review ends here: what the analysis is estimated to cost (or why there is no
  // estimate), and the launch button.
  import { estimateLine } from '../estimate';
  import { launchForm } from './launchForm.svelte';

  const form = launchForm();
</script>

<div class="launch-card">
  <p class="estimate-line">
    {estimateLine({
      text: form.estimate.text,
      estimating: form.estimate.estimating,
      failed: form.estimate.failed,
      samples: form.samples,
    })}
  </p>
  <button
    class="btn btn-compact btn-primary launch big"
    onclick={form.launch}
    disabled={form.launchDisabled}
  >
    {form.launching ? 'Launching…' : form.from?.resume ? 'Resume analysis →' : 'Launch analysis →'}
  </button>
</div>

<style>
  .launch-card {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.9rem 1rem;
    margin-bottom: 0.9rem;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
    background: var(--hover);
  }
  .estimate-line {
    margin: 0;
    font-size: 0.85rem;
    color: var(--text-muted);
  }
  .launch {
    font-size: 0.9rem;
    padding: 0.45rem 1rem;
  }
  .launch.big {
    font-size: 1rem;
    padding: 0.6rem 1.2rem;
  }
  @media (max-width: 800px) {
    .launch-card {
      flex-direction: column;
      align-items: stretch;
    }
  }
</style>
