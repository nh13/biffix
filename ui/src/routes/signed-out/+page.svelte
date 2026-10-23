<!--
  Where Cognito returns the browser after ending the session (a registered logout URL).

  This page exists so signing out is possible at all: the app authenticates on mount, so returning
  to "/" would immediately redirect back to the IdP and — with the Cognito session already gone —
  present a login screen the user did not ask for, or worse, silently re-authenticate. Landing
  somewhere distinct lets the app stay signed out until the user chooses otherwise.
-->
<script lang="ts">
  import { beginLogin, loadOAuthConfig } from '$lib/api/auth';
  import { errorMessage } from '$lib/util/format';

  let error = $state<string | null>(null);
  let busy = $state(false);

  async function signIn() {
    busy = true;
    error = null;
    try {
      window.location.assign(await beginLogin(await loadOAuthConfig(), '/'));
    } catch (e) {
      error = `Sign-in couldn't start: ${errorMessage(e) || 'the sign-in service is unavailable'}`;
      busy = false;
    }
  }
</script>

<svelte:head><title>Signed out · biffix</title></svelte:head>

<div class="wrap">
  <div class="signed-out">
    <span class="brand">biffix</span>
    <h1>You're signed out</h1>
    <p>Your session on this device has ended.</p>
    <button onclick={signIn} disabled={busy}>{busy ? 'Redirecting…' : 'Sign in'}</button>
    {#if error}<p class="error" role="alert">{error}</p>{/if}
  </div>
</div>

<style>
  .wrap {
    display: grid;
    place-items: center;
    min-height: 100vh;
    padding: 2rem;
  }
  .signed-out {
    display: flex;
    flex-direction: column;
    gap: 0.75rem;
    align-items: flex-start;
    max-width: 22rem;
  }
  .brand {
    font-weight: 700;
    letter-spacing: 0.02em;
  }
  h1 {
    margin: 0;
    font-size: 1.25rem;
  }
  p {
    margin: 0;
    opacity: 0.75;
  }
  button {
    margin-top: 0.5rem;
    padding: 0.5rem 1rem;
    border-radius: 0.5rem;
    border: 1px solid currentColor;
    background: transparent;
    color: inherit;
    cursor: pointer;
    font: inherit;
  }
  button:disabled {
    opacity: 0.6;
    cursor: default;
  }
</style>
