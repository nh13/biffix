<script lang="ts">
  import { page } from '$app/state';
  import { goto } from '$app/navigation';
  import { workflowTabHref } from '#lib/workflows/workflowNav.js';

  // The version's own URL lands on its first tab, Run.
  $effect(() => {
    const id = page.params.id ?? '';
    if (id) goto(workflowTabHref(id, 'run'), { replace: true });
  });
</script>
