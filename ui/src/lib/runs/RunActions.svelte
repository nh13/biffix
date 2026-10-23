<script lang="ts">
  import { getProvenance, type RunDetail } from '$lib/api';
  import { saveBlob } from '$lib/util/download';
  import { isTerminalRun } from '$lib/runs/runState';
  import { getRunContext } from '$lib/runs/runContext.svelte';
  import { workflowTabHref } from '$lib/workflows/workflowNav';
  import { errorMessage } from '$lib/util/format';

  // What can be done to the run from anywhere on its page: Cancel while it is live; once it has
  // ended, Re-run it (and Resume a Nextflow run from its cache). Both open the workflow's Run tab
  // pre-filled from this run, so a re-run is a review-and-change, not a blind resubmit.
  //
  // The page shows these buttons twice (the header, and the pinned bar once the header scrolls
  // away), so what they are doing is the run context's, shared: one Cancel in flight disables both.
  interface Props {
    run: RunDetail;
    /** Announce a failure (role=alert); only one of the copies does, so it is read out once. */
    announce?: boolean;
  }
  let { run, announce = true }: Props = $props();

  const actions = getRunContext().actions;

  // Cancel: terminates the run's coordinator; the poll then shows the run ending as cancelled.
  const cancel = () =>
    actions.cancel(run.run_id, `Cancel ${run.run_id}? Its running jobs will be terminated.`);

  // Provenance: the run's Workflow Run RO-Crate, built from its log now, saved as a file. (A copy
  // is also written into the run's S3 folder once it has ended.)
  async function downloadProvenance() {
    if (actions.savingProvenance) return;
    actions.savingProvenance = true;
    actions.error = null;
    try {
      saveBlob(await getProvenance(run.run_id), `${run.run_id}-ro-crate-metadata.json`);
    } catch (e) {
      actions.error = `Could not download provenance: ${errorMessage(e)}`;
    } finally {
      actions.savingProvenance = false;
    }
  }

  const ended = $derived(isTerminalRun(run));
  // The server's own resume check (`RunDetail.resumable`), so Resume shows exactly when it works.
  const resumable = $derived(run.resumable === true);
  const formHref = (workflow: string, resume: boolean) =>
    `${workflowTabHref(workflow, 'run')}?from=${encodeURIComponent(run.run_id)}${resume ? '&resume=1' : ''}`;
</script>

<span class="actions">
  {#if !ended}<button class="cancel" onclick={cancel} disabled={actions.cancelling}
      >{actions.cancelling ? 'Cancelling…' : 'Cancel run'}</button
    >{/if}
  {#if ended && run.workflow}
    {#if resumable}<a
        class="action primary"
        href={formHref(run.workflow, true)}
        title="Start a new run from this run's cache: finished tasks are not run again">Resume</a
      >{/if}
    <a
      class="action"
      href={formHref(run.workflow, false)}
      title="Open the launch form with this run's workflow version and parameters">Re-run</a
    >
  {/if}
  {#if ended}<button
      class="action"
      onclick={downloadProvenance}
      disabled={actions.savingProvenance}
      title="Download this run's provenance as a Workflow Run RO-Crate (inputs, parameters, software, every step and its outputs)"
      >{actions.savingProvenance ? 'Preparing…' : 'Provenance'}</button
    >{/if}
  {#if actions.error}<span class="cancel-error" role={announce ? 'alert' : undefined}
      >{actions.error}</span
    >{/if}
</span>

<style>
  .actions {
    display: inline-flex;
  }
  .cancel {
    font-size: 0.75rem;
    padding: 0.15rem 0.6rem;
    border: 1px solid #e5484d55;
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--fg-fail);
    cursor: pointer;
    white-space: nowrap;
  }
  .cancel:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .action {
    text-decoration: none;
    font-size: 0.75rem;
    padding: 0.15rem 0.6rem;
    border: 1px solid var(--border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    cursor: pointer;
    white-space: nowrap;
  }
  .action.primary {
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    border-color: var(--tab-active-bg);
  }
  .action:disabled {
    opacity: 0.6;
    cursor: default;
  }
  .cancel-error {
    font-size: 0.75rem;
    color: var(--fg-fail);
  }
</style>
