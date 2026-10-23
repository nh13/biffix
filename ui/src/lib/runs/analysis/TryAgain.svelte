<script lang="ts">
  // *Try again* for a failed or cancelled analysis. A resume or re-run asks first,
  // naming what will happen, then launches the run the launch form would launch for `?from=`; a
  // fix mode opens that form instead, at the step to fix. A run the form would not launch untouched
  // (a secret to type again, a reference to choose, a value it reports) also opens the form. The
  // server checks the launch permission, as it does for the Expert view's Re-run and Resume.
  import { goto } from '$app/navigation';
  import { runTabHref } from '$lib/runs/runNav';
  import {
    getWorkflow,
    listExperiments,
    listReferences,
    submitRun,
    type ReferenceBundle,
    type RunDetail,
    type WorkflowDetail,
  } from '$lib/api';
  import { parseSchema } from '$lib/workflows/schema';
  import {
    fixHref,
    isFixMode,
    launchFormHref,
    tryAgainHint,
    tryAgainLabel,
    tryAgainRequest,
    type TryAgainMode,
  } from './tryAgain';
  import { errorMessage } from '$lib/util/format';
  import Dialog from '$lib/components/Dialog.svelte';

  interface Props {
    run: RunDetail;
    mode: TryAgainMode;
    /** The run's workflow version, when the page has loaded it (else it is loaded on confirm). */
    version?: WorkflowDetail | null;
    /** The filled button (the page's main action) rather than an outlined one. */
    primary?: boolean;
    /** Show what it will do under the button. */
    hint?: boolean;
    /** For a fix-samples mode, the failing sample the launch form highlights. */
    sample?: string | null;
  }
  let { run, mode, version = null, primary = false, hint = false, sample = null }: Props = $props();

  const uid = $props.id();
  let asking = $state(false);
  let launching = $state(false);
  let error = $state<string | null>(null);

  const versionId = $derived(run.workflow ?? '');
  const resume = $derived(mode === 'resume');

  function start() {
    if (isFixMode(mode)) {
      void goto(fixHref(mode, versionId, run.run_id, sample));
      return;
    }
    error = null;
    asking = true;
  }

  /** The bundles the form's reference dropdown offers, as it asks for them (`null` if none
   * load). */
  async function offeredBundles(detail: WorkflowDetail): Promise<ReferenceBundle[] | null> {
    const refs = parseSchema(detail.form_schema).references;
    if (!refs) return null;
    try {
      return await listReferences({ kind: refs.kind, keys: refs.required });
    } catch {
      // Judged as the form judges it while its bundles load.
      return null;
    }
  }

  /**
   * The ids of the experiments open in the version's project, as the form offers them, when the run
   * is in one (else none are needed); `null` when they do not load, so the form judges it.
   */
  async function openExperiments(detail: WorkflowDetail): Promise<string[] | null> {
    if (!run.experiment_id) return [];
    if (!detail.project) return null;
    try {
      const { experiments } = await listExperiments({ project: detail.project });
      return experiments.filter((e) => e.archived_at == null).map((e) => e.id);
    } catch {
      return null;
    }
  }

  async function confirm() {
    if (launching) return;
    launching = true;
    error = null;
    try {
      const detail = version ?? (await getWorkflow(versionId));
      const [offered, open] = await Promise.all([offeredBundles(detail), openExperiments(detail)]);
      const request = tryAgainRequest(run, detail, resume, offered, open);
      if (!request) {
        // The form would not launch it untouched (or would drop its experiment): it shows why,
        // pre-filled with everything else.
        asking = false;
        await goto(launchFormHref(versionId, run.run_id, { resume }));
        return;
      }
      const { run_id } = await submitRun(request);
      asking = false;
      await goto(runTabHref(run_id, 'overview'));
    } catch (e) {
      error = errorMessage(e);
    } finally {
      launching = false;
    }
  }
</script>

<span class="try-again">
  <button class="btn" class:btn-primary={primary} onclick={start} disabled={!versionId}
    >{tryAgainLabel(mode)}</button
  >
  {#if hint}<span class="hint">{tryAgainHint(mode)}</span>{/if}
</span>

{#if asking}
  <Dialog
    labelledby="try-again-{uid}"
    onclose={() => {
      if (!launching) asking = false;
    }}
    size="confirm"
    closeOnBackdrop={false}
  >
    <h2 id="try-again-{uid}">
      {resume ? 'Resume this analysis?' : 'Run this analysis again?'}
    </h2>
    <p>{tryAgainHint(mode)}.</p>
    <p class="muted">
      It starts as a new analysis; this one stays as it is. Want to change something first?
      <a href={launchFormHref(versionId, run.run_id, { resume })}>Open the launch form</a>.
    </p>
    {#if error}<p class="error" role="alert">Couldn't start it: {error}</p>{/if}
    <div class="buttons">
      <button class="btn" onclick={() => (asking = false)} disabled={launching}>Not now</button>
      <button class="btn btn-primary" onclick={confirm} disabled={launching}
        >{launching ? 'Starting…' : resume ? 'Resume' : 'Run again'}</button
      >
    </div>
  </Dialog>
{/if}

<style>
  .try-again {
    display: inline-flex;
    flex-direction: column;
    align-items: flex-start;
    gap: 0.25rem;
  }
  /* Bolder than the shared primary button: on a failed analysis it is the way forward. */
  .btn-primary {
    font-weight: 600;
  }
  .hint {
    max-width: 16rem;
  }
  h2 {
    margin: 0 0 0.5rem;
    font-size: 1rem;
  }
  p {
    margin: 0 0 0.6rem;
    font-size: 0.88rem;
  }
  .muted a {
    color: var(--accent);
    text-decoration: underline;
  }
  .buttons {
    display: flex;
    justify-content: flex-end;
    gap: 0.5rem;
    margin-top: 0.8rem;
  }
</style>
