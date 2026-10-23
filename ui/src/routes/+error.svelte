<script lang="ts">
  // A route the app does not have (or one that threw): said in the view's words with a way back,
  // the status and message behind *Details*.
  import { page } from '$app/state';
  import ErrorPage from '$lib/components/ErrorPage.svelte';
  import { errorKind } from '$lib/components/errorPage';

  const kind = $derived(errorKind(page.status));
  const detail = $derived(
    `${page.status}${page.error?.message ? ` ${page.error.message}` : ''} (${page.url.pathname})`,
  );
</script>

<ErrorPage {kind} subject="page" {detail} onretry={() => location.reload()} />
