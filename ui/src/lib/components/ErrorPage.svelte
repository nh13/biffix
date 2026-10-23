<script lang="ts">
  // A page that could not be shown: what happened in the view's words, a way back, Try again
  // when it may help, and the technical line behind *Details* (both views keep it there).
  import { signInAgain } from '#lib/api/auth.js';
  import { errorWords, type ErrorKind, type ErrorSubject } from '#lib/components/errorPage.js';
  import { view } from '#lib/state/userPrefs.svelte.js';

  interface Props {
    kind: ErrorKind;
    subject: ErrorSubject;
    /** The technical line (the server's message, the request). */
    detail?: string | null;
    /** Offered when trying again may help; absent: no Try again. */
    onretry?: () => void;
  }
  let { kind, subject, detail = null, onretry }: Props = $props();

  const words = $derived(errorWords(kind, subject, view.mode));
</script>

<!-- A labelled region; only what went wrong is announced (an alert reads as one string). -->
<section class="error-page" aria-labelledby="error-page-title" data-kind={kind}>
  <h1 id="error-page-title">{words.title}</h1>
  <p class="body" role="alert">{words.body}</p>
  <p class="actions">
    {#if words.signIn}<button type="button" class="retry" onclick={() => void signInAgain()}
        >Sign in</button
      >{/if}
    <a class="back" href={words.back.href}>{words.back.label} →</a>
    {#if words.retry && onretry}<button type="button" class="retry" onclick={onretry}
        >Try again</button
      >{/if}
  </p>
  {#if detail}
    <details class="details">
      <summary>Details</summary>
      <code>{detail}</code>
    </details>
  {/if}
</section>

<style>
  .error-page {
    max-width: 36rem;
    margin: 2rem 0;
    padding: 1.25rem 1.4rem;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
    background: var(--bg-content);
  }
  h1 {
    margin: 0 0 0.5rem;
    font-size: 1.25rem;
  }
  .body {
    margin: 0 0 1rem;
    color: var(--text);
  }
  /* Wider apart than the shared row: a text link beside a button. */
  .actions {
    gap: 0.75rem;
    margin: 0;
  }
  .back {
    color: var(--accent);
    font-weight: 600;
  }
  .retry {
    padding: 0.35rem 0.85rem;
    border: 1px solid var(--control-border);
    border-radius: 0.45rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .retry:hover {
    background: var(--hover);
  }
  .back:focus-visible,
  .retry:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  .details {
    margin-top: 1rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .details code {
    display: block;
    margin-top: 0.4rem;
    overflow-wrap: anywhere;
    white-space: pre-wrap;
  }
</style>
