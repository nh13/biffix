<script lang="ts">
  // One experiment. Polled while any of its analyses is running. The Expert view is
  // the same page in its words, with run ids (ExperimentPage).
  import { page } from '$app/state';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import ErrorPage from '#lib/components/ErrorPage.svelte';
  import { ExperimentLoader } from '#lib/experiments/experimentLoader.svelte.js';
  import ExperimentPage from '#lib/experiments/ExperimentPage.svelte';

  const id = $derived(page.params.id ?? '');
  const loader = new ExperimentLoader();
  $effect(() => loader.watch(id));
</script>

<a class="back-link" href="/experiments">← Experiments</a>

{#if loader.loading && !loader.detail}
  <p class="muted">Loading…</p>
{:else if loader.error && !loader.detail}
  <ErrorPage
    kind={loader.errorKind}
    subject="experiment"
    detail={loader.error}
    onretry={() => loader.retry()}
  />
{:else if loader.detail}
  <ExperimentPage
    detail={loader.detail}
    mode={view.mode}
    onupdated={(experiment) => loader.updated(experiment)}
  />
{/if}
