<script lang="ts">
  // An unregistered workflow's launch form (`biffix workflows form --open`). The form travels in the
  // URL fragment, so it is decoded here and never sent to the server.
  import { onMount } from 'svelte';
  import { decodePreview } from '$lib/workflows/preview';
  import { errorMessage } from '$lib/util/format';
  import type { LaunchTarget } from '$lib/launch/form';
  import RunForm from '$lib/launch/RunForm.svelte';

  let detail = $state<LaunchTarget | null>(null);
  let error = $state<string | null>(null);

  // Decoding is async, so two quick link changes could finish out of order: only the latest counts.
  let latest = 0;
  async function load() {
    const seq = ++latest;
    detail = null;
    error = null;
    const payload = window.location.hash.slice(1);
    if (!payload) {
      error = 'This link has no form in it. Run `biffix workflows form <path> --open` to make one.';
      return;
    }
    try {
      const p = await decodePreview(payload);
      if (seq !== latest) return;
      detail = { id: 'preview', name: p.name, engine: p.engine, form_schema: p.form_schema };
    } catch (e) {
      if (seq === latest) error = `Couldn't read the form in this link: ${errorMessage(e)}`;
    }
  }
  onMount(() => {
    load();
    window.addEventListener('hashchange', load);
    return () => window.removeEventListener('hashchange', load);
  });
</script>

<a class="back-link" href="/workflows">← Workflows</a>
{#if error}
  <p class="error" role="alert">{error}</p>
{:else if detail}
  <header>
    <h1>{detail.name}</h1>
    <span class="tag">{detail.engine}</span>
    <span class="tag">preview</span>
  </header>
  {#key detail}
    <RunForm {detail} preview />
  {/key}
{:else}
  <p class="muted">Loading…</p>
{/if}

<style>
  header {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    flex-wrap: wrap;
    margin-bottom: 1rem;
  }
  h1 {
    margin: 0 0.25rem 0 0;
  }
  .tag {
    font-size: 0.72rem;
    padding: 0.05rem 0.45rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
</style>
