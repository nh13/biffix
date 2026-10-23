<script lang="ts">
  // A version's release: whether it is released, who released it and when (the detail's own
  // `release`, so the page reads it once), and whether the viewer may change it (`can_release`: a
  // Developer or Admin; others see the badge only). A change asks first; its answer wins until the
  // version changes. A release that cannot be read fails the detail itself, so the page shows the
  // error.
  import { putWorkflowRelease, type VersionRelease, type WorkflowDetail } from '#lib/api/index.js';
  import { errorMessage, fmtRelativeMicros } from '#lib/util/format.js';
  import type { WorkflowState } from './workflowContext.svelte';
  import ReleaseBadge from './ReleaseBadge.svelte';

  interface Props {
    wf: WorkflowState;
    detail: WorkflowDetail;
    /** The version on show (the route's id). */
    id: string;
  }
  let { wf, detail, id }: Props = $props();

  let releaseOverride = $state<VersionRelease | null>(null);
  let confirmingRelease = $state(false);
  let releasing = $state(false);
  let releaseError = $state<string | null>(null);
  $effect(() => {
    void id;
    releaseOverride = null;
    confirmingRelease = false;
    releaseError = null;
  });
  const release = $derived(releaseOverride ?? detail.release ?? null);
  const released = $derived(release?.released ?? false);
  async function setReleased(next: boolean) {
    const wid = id;
    releasing = true;
    releaseError = null;
    try {
      const r = await putWorkflowRelease(wid, next);
      if (wid !== id) return;
      releaseOverride = r.release ?? { released: false };
      confirmingRelease = false;
      // The version picker (and the workflow's other pages) read the listed versions.
      wf.versions = wf.versions.map((v) => (v.id === wid ? { ...v, release: r.release } : v));
    } catch (e) {
      releaseError = errorMessage(e);
    } finally {
      releasing = false;
    }
  }
</script>

<div class="release-ctl" data-testid="release-ctl">
  <ReleaseBadge {released} />
  {#if release?.released && release.at_micros != null}
    <span class="dim"
      >released {fmtRelativeMicros(release.at_micros)}{(release.by_label ?? release.by)
        ? ` by ${release.by_label ?? release.by}`
        : ''}</span
    >
  {/if}
  {#if detail.can_release}
    {#if confirmingRelease}
      <span class="confirm" role="group" aria-label="Confirm the release change">
        <span
          >{released
            ? 'Un-release this version? It becomes a developer version, and the catalog launches the newest released version instead.'
            : 'Release this version? The catalog will show this pipeline by default and launch this version unless a newer one is released.'}</span
        >
        <button class="confirm-btn" disabled={releasing} onclick={() => setReleased(!released)}
          >{released ? 'Yes, un-release' : 'Yes, release'}</button
        >
        <button class="link" disabled={releasing} onclick={() => (confirmingRelease = false)}
          >Cancel</button
        >
      </span>
    {:else}
      <button class="link" onclick={() => (confirmingRelease = true)}
        >{released ? 'Un-release this version' : 'Release this version'}</button
      >
    {/if}
  {/if}
  {#if releaseError}<span class="error" role="alert">{releaseError}</span>{/if}
</div>

<style>
  .release-ctl {
    display: flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
    margin-top: 0.35rem;
    font-size: 0.78rem;
  }
  .release-ctl .dim {
    color: var(--text-muted);
  }
  .confirm {
    display: inline-flex;
    align-items: center;
    flex-wrap: wrap;
    gap: 0.5rem;
  }
  .confirm-btn {
    font: inherit;
    padding: 0.1rem 0.6rem;
    border-radius: 0.4rem;
    border: 1px solid var(--tab-active-bg);
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    cursor: pointer;
  }
  .confirm-btn:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .link:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
