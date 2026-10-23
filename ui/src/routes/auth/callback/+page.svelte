<script lang="ts">
  import { goto } from '$app/navigation';
  import { completeLogin } from '$lib/api/auth';
  import { errorMessage } from '$lib/util/format';
  import { onMount } from 'svelte';

  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      const returnTo = await completeLogin(new URL(window.location.href));
      await goto(returnTo, { replaceState: true });
    } catch (reason) {
      error = errorMessage(reason);
    }
  });
</script>

<svelte:head><title>Signing in · biffix</title></svelte:head>

<section class="auth-callback" aria-live="polite">
  {#if error}
    <h1>Sign-in failed</h1>
    <p>{error}</p>
    <!-- A full load: the app shell then signs in afresh, rather than skipping its auth check. -->
    <a href="/" data-sveltekit-reload>Try again</a>
  {:else}
    <h1>Signing in</h1>
    <p>Completing Cognito authentication…</p>
  {/if}
</section>

<style>
  .auth-callback {
    max-width: 36rem;
    margin: 20vh auto;
  }
</style>
