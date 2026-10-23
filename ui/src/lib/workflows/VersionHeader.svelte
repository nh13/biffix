<script lang="ts">
  // A workflow version's header: its name (the Lab view's friendly title, description and what you
  // get), its tags (project, engine, state, the frozen id to copy), its release, whether the Lab
  // catalog hides it (Expert view), who registered it, and the version picker.
  import { putWorkflowListing, type WorkflowDetail } from '$lib/api';
  import { errorMessage, fmtRelativeMicros } from '$lib/util/format';
  import { CopiedFlag } from '$lib/util/clipboard.svelte';
  import VerifiedBadge from '$lib/catalog/VerifiedBadge.svelte';
  import WhatYouGet from '$lib/catalog/WhatYouGet.svelte';
  import { readinessOf, stateChip } from './readiness';
  import { engineLabel, versionLabel, registrantLabel as who, workflowKey } from './version';
  import type { WorkflowState } from './workflowContext.svelte';
  import type { WorkflowTab } from './workflowNav';
  import ReleaseControl from './ReleaseControl.svelte';
  import VersionPicker from './VersionPicker.svelte';

  interface Props {
    wf: WorkflowState;
    detail: WorkflowDetail;
    /** The version on show (the route's id). */
    id: string;
    /** The tab on show, which the version picker keeps. */
    tab: WorkflowTab;
    /** The Lab view: what the pipeline is for, not its registry details. */
    lab: boolean;
  }
  let { wf, detail, id, tab, lab }: Props = $props();

  const chip = $derived(stateChip(readinessOf(detail)));
  const copied = new CopiedFlag();

  // What the listing says about this version (its description with the server's fallbacks, the
  // Verified mark), so the page and its catalog card agree.
  const listed = $derived(wf.versions.find((v) => v.id === id));
  const catalog = $derived(listed?.catalog);
  // The pipeline's friendly title (its form's), the Lab's name for it; else its registry name.
  const labTitle = $derived.by(() => {
    const t = (detail.form_schema as { title?: unknown } | undefined)?.title;
    return (typeof t === 'string' && t.trim()) || detail.name || '';
  });

  // Whether the pipeline is hidden from the Lab catalog (read off its newest listed version), and
  // the developer's switch for it.
  // The server's answer to the last switch wins; until the list loads the state is unknown.
  // Keyed by the two strings, not `detail`, which the layout's poll replaces while the version
  // builds.
  let hiddenOverride = $state<boolean | null>(null);
  const project = $derived(detail.project);
  const name = $derived(detail.name);
  $effect(() => {
    void project;
    void name;
    hiddenOverride = null;
  });
  const hidden = $derived(hiddenOverride ?? wf.versions[0]?.hidden ?? false);
  let hiding = $state(false);
  let hideError = $state<string | null>(null);
  async function setHidden(next: boolean) {
    // An answer that lands after a move to another workflow is that one's no longer (as
    // ReleaseControl guards its release).
    const key = workflowKey(detail);
    hiding = true;
    hideError = null;
    try {
      const hiddenNow = (await putWorkflowListing(detail.project, detail.name, next)).hidden;
      if (workflowKey(detail) === key) hiddenOverride = hiddenNow;
    } catch (e) {
      if (workflowKey(detail) === key) hideError = errorMessage(e);
    } finally {
      hiding = false;
    }
  }
</script>

<header class="wf-header">
  <div>
    <h1>
      {lab ? labTitle : detail.name}
      {#if catalog?.verified}<VerifiedBadge />{/if}
    </h1>
    {#if lab && labTitle !== detail.name}<p class="repo" data-testid="pipeline-repo">
        {detail.name}
      </p>{/if}
    {#if lab && catalog?.description}<p class="lab-desc">{catalog.description}</p>{/if}
    {#if lab}<div class="lab-get">
        <WhatYouGet {catalog} typical={listed?.typical} />
      </div>{/if}
    <div class="tags">
      <span class="tag project" class:lab-hidden={lab}>{detail.project}</span>
      <span class="tag engine" class:lab-hidden={lab}>{engineLabel(detail)}</span>
      {#if chip}
        <span class="tag state {chip.cls}" data-testid="version-state">{chip.label}</span>
      {/if}
      <button
        class="id"
        class:lab-hidden={lab}
        title="Frozen version id — click to copy"
        onclick={() => copied.copy(detail.id)}
      >
        {versionLabel(detail)}{copied.is(detail.id) ? ' · copied' : ''}
      </button>
    </div>
    <ReleaseControl {wf} {detail} {id} />
    {#if !lab}
      <div class="catalog-ctl">
        {#if hidden}<span class="tag state other">hidden from the Lab catalog</span>{/if}
        <button
          class="link"
          disabled={hiding || wf.versions.length === 0}
          onclick={() => setHidden(!hidden)}
          >{hidden ? 'Show in the Lab catalog' : 'Hide from the Lab catalog'}</button
        >
        {#if hideError}<span class="error" role="alert">{hideError}</span>{/if}
      </div>
    {/if}
    <div class="registered" class:lab-hidden={lab}>
      registered {fmtRelativeMicros(detail.created_at_micros)}{who(detail)
        ? ` by ${who(detail)}`
        : ''}
    </div>
  </div>

  {#if wf.versions.length > 1}
    <VersionPicker versions={wf.versions} {id} {tab} />
  {/if}
</header>

<style>
  .wf-header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    flex-wrap: wrap;
  }
  h1 {
    margin: 0;
  }
  .repo {
    margin: 0.15rem 0 0;
    font-family: ui-monospace, monospace;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
  .lab-desc {
    margin: 0.35rem 0 0;
    color: var(--text-muted);
    max-width: 48rem;
  }
  .lab-get {
    margin: 0.5rem 0 0.3rem;
    max-width: 44rem;
  }
  .lab-hidden {
    display: none;
  }
  .catalog-ctl {
    display: flex;
    align-items: center;
    gap: 0.5rem;
    margin-top: 0.35rem;
    font-size: 0.78rem;
  }
  .link:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .tags {
    display: flex;
    align-items: center;
    gap: 0.4rem;
    flex-wrap: wrap;
    margin-top: 0.35rem;
  }
  .tag.state.building,
  .tag.state.other {
    color: var(--fg-warn);
    border-color: currentColor;
  }
  .tag.state.failed {
    color: var(--fg-fail);
    border-color: currentColor;
  }
  .tag {
    font-size: 0.72rem;
    padding: 0.05rem 0.45rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
  .id {
    font-family: var(--mono);
    font-size: 0.72rem;
    padding: 0.05rem 0.45rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
    background: var(--bg-content);
    color: var(--text-muted);
    cursor: pointer;
  }
  .registered {
    margin-top: 0.35rem;
    font-size: 0.78rem;
    color: var(--text-muted);
  }
</style>
