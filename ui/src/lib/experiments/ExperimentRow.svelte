<script lang="ts">
  // One experiment as a My work row: its name, its project and when it was last active, how many
  // analyses it holds and how many of the caller's need a look, and Open.
  import type { Experiment } from '#lib/api/index.js';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import { activityText, attentionText, experimentHref } from './experiments';
  import WorkItem from '#lib/home/WorkItem.svelte';

  let { experiment }: { experiment: Experiment } = $props();
  const attention = $derived(attentionText(experiment));
</script>

<WorkItem
  href={experimentHref(experiment.id)}
  name={experiment.name}
  tone={attention ? 'attention' : ''}
  flag={attention}
>
  {#snippet sub()}{experiment.project} · {activityText(experiment, view.mode)}{/snippet}
</WorkItem>
