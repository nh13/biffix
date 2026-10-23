<script lang="ts">
  import { getWorkflowContext } from '#lib/workflows/workflowContext.svelte.js';
  import { workflowTabHref } from '#lib/workflows/workflowNav.js';
  import { parseSchema } from '#lib/workflows/schema.js';
  import { firstParagraph } from '#lib/markdown/markdown.js';
  import RunForm from '#lib/launch/RunForm.svelte';
  import { canRun, readinessOf } from '#lib/workflows/readiness.js';
  import { versionLabel } from '#lib/workflows/version.js';
  import { view } from '#lib/state/userPrefs.svelte.js';

  const wf = getWorkflowContext();

  // What the workflow does, above its form: the author's form description, else the About page's
  // first paragraph.
  const description = $derived(
    wf.detail ? parseSchema(wf.detail.form_schema).description?.trim() || null : null,
  );
  const readiness = $derived(wf.detail ? readinessOf(wf.detail) : null);
  // The Lab view launches through the step-by-step wizard (the same form, rendered a step at a
  // time); its header already says what the pipeline does.
  const lab = $derived(view.mode === 'lab');
  const aboutIntro = $derived(
    !description && wf.detail?.about_markdown ? firstParagraph(wf.detail.about_markdown) : null,
  );
</script>

{#if wf.detail}
  {#if !lab && (description || aboutIntro || wf.detail.about_markdown)}
    <div class="intro">
      {#if description}
        <p>{description}</p>
      {:else if aboutIntro}
        <!-- Sanitized by firstParagraph. -->
        {@html aboutIntro}
      {/if}
      {#if wf.detail.about_markdown}
        <a href={workflowTabHref(wf.detail.id, 'about')}>More about this workflow →</a>
      {/if}
    </div>
  {/if}
  {#if readiness && !canRun(readiness)}
    <div class="not-ready {readiness.state}" data-testid="not-ready">
      <h3>
        {readiness.state === 'failed'
          ? 'This version cannot run: part of it failed to build'
          : readiness.state === 'other'
            ? `This version cannot run now (${readiness.label})`
            : 'This version is being prepared; it can run once it is built'}
      </h3>
      <ul>
        {#each readiness.reasons as reason (reason)}<li>{reason}</li>{/each}
      </ul>
      <a href={workflowTabHref(wf.detail.id, 'provenance')}>Engine and images →</a>
    </div>
  {:else}
    <RunForm detail={wf.detail} wizard={lab} versionText={versionLabel(wf.detail)} />
  {/if}
{/if}

<style>
  .not-ready {
    max-width: 48rem;
    padding: 0.8rem 1rem;
    border: 1px solid var(--border);
    border-left: 3px solid var(--fg-warn);
    border-radius: 0.5rem;
    background: var(--bg-content);
  }
  .not-ready.failed {
    border-left-color: var(--fg-fail);
  }
  .not-ready h3 {
    margin: 0 0 0.4rem;
    font-size: 0.95rem;
  }
  .not-ready ul {
    margin: 0 0 0.5rem;
    padding-left: 1.2rem;
    font-size: 0.85rem;
    color: var(--text-muted);
    word-break: break-word;
  }
  .not-ready a {
    color: var(--accent);
    font-size: 0.85rem;
  }
  .intro {
    max-width: 48rem;
    margin: 0 0 1rem;
    color: var(--text-muted);
    font-size: 0.9rem;
    line-height: 1.5;
  }
  .intro :global(p) {
    margin: 0 0 0.3rem;
  }
  .intro a,
  .intro :global(a) {
    color: var(--accent);
    font-size: 0.85rem;
  }
</style>
