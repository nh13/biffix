<script lang="ts">
  // The Run tab: a biologist-first launch form for one workflow version. Cards in
  // the author's section order, advanced sections collapsed together, a *Run details* card, and a
  // sticky footer that names what is left to fix and launches. The same version's params YAML can
  // be imported or downloaded, so the form and `biffix runs submit --params` stay interchangeable.
  //
  // The Lab view renders the same form as a wizard (`wizard`): the same state,
  // draft, validation and `POST /runs`, one step at a time (`?step=2..4`) with a Review before
  // launch. Switching view re-renders this one component, so nothing entered is lost.
  //
  // This is the shell: it owns the form's state (`LaunchForm`, which its parts read from context)
  // and picks the layout, `FormLayout` or `WizardLayout`, under the banners both share.
  import { setDataProject } from '$lib/data/projectContext';
  import type { LaunchTarget } from './form';
  import FormLayout from './form/FormLayout.svelte';
  import LaunchBanners from './form/LaunchBanners.svelte';
  import WizardLayout from './form/WizardLayout.svelte';
  import { LaunchForm, setLaunchForm } from './form/launchForm.svelte';

  interface Props {
    detail: LaunchTarget;
    /** An unregistered form (`biffix workflows form --open`): fill it in, but never launch or
     * save. */
    preview?: boolean;
    /** Render as the Lab view's step-by-step wizard rather than the single long form. */
    wizard?: boolean;
    /** The version as the wizard's Review names it (e.g. its version label). */
    versionText?: string | null;
  }
  let { detail, preview = false, wizard = false, versionText = null }: Props = $props();

  const form = new LaunchForm({
    get detail() {
      return detail;
    },
    get preview() {
      return preview;
    },
    get wizard() {
      return wizard;
    },
  });
  setLaunchForm(form);
  // Its file pickers browse and upload into the version's project (its bucket and data sources).
  setDataProject(() => form.project);
</script>

<svelte:window onkeydown={form.keydown} />

<div class="run-form">
  <LaunchBanners />
  {#if wizard}
    <WizardLayout {versionText} />
  {:else}
    <FormLayout />
  {/if}
</div>

<style>
  .run-form {
    max-width: 60rem;
  }
</style>
