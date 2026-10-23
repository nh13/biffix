<script lang="ts">
  // The upload tray: every upload of this tab with its progress, pause, resume
  // and cancel; uploads a reload interrupted, which resume once their file is chosen again; and
  // uploads another tab is sending, for information only.
  import { fmtBytes } from '$lib/util/format';
  import { uploads } from './store';
  import type { SavedUpload, Upload, UploadState } from './uploads.svelte';

  let collapsed = $state(false);
  // In progress: moving, or paused mid-way (a failed upload is not).
  const active = $derived(
    uploads.list.filter((u) => ['uploading', 'paused', 'completing'].includes(u.state)),
  );
  const shown = $derived(
    uploads.list.length + uploads.interrupted.length + uploads.elsewhere.length > 0,
  );
  let reselectError = $state<string | null>(null);

  // Screen readers hear each upload's state changes (not every percent): uploaded, failed…
  let announcement = $state('');
  const announced = new Map<string, UploadState>();
  $effect(() => {
    for (const u of uploads.list) {
      const now = u.state;
      const before = announced.get(u.saved.upload_id);
      announced.set(u.saved.upload_id, now);
      if (before !== undefined && before !== now && now !== 'completing')
        announcement = `${u.name}: ${now === 'uploading' ? 'uploading' : status(u)}`;
    }
  });

  function percent(u: Upload): number {
    return Math.floor(u.fraction * 100);
  }
  function status(u: Upload): string {
    switch (u.state) {
      case 'uploading':
        return `${percent(u)}% · ${fmtBytes(u.rate)}/s`;
      case 'paused':
        return `Paused at ${percent(u)}%`;
      case 'completing':
        return 'Finishing…';
      case 'done':
        return 'Uploaded';
      case 'cancelled':
        return 'Cancelled';
      case 'failed':
        return u.error ?? 'Failed';
    }
  }
  async function reselect(saved: SavedUpload, e: Event) {
    const input = e.currentTarget as HTMLInputElement;
    const file = input.files?.[0];
    input.value = '';
    if (!file) return;
    const result = await uploads.resumeWith(saved, file);
    reselectError =
      result === 'different-file'
        ? `${file.name} is not the file this upload started from (${saved.name}, ${fmtBytes(saved.size)}).`
        : null;
  }
</script>

{#if shown}
  <section class="tray" aria-label="Uploads">
    <header>
      <h2>
        Uploads{#if active.length > 0}<span class="count"> · {active.length} in progress</span>{/if}
      </h2>
      <div class="head-actions">
        {#if uploads.list.some((u) => u.finished)}
          <button class="link" onclick={() => uploads.clearFinished()}>Clear finished</button>
        {/if}
        <button
          class="icon"
          aria-expanded={!collapsed}
          aria-label={collapsed ? 'Show uploads' : 'Hide uploads'}
          onclick={() => (collapsed = !collapsed)}>{collapsed ? '▴' : '▾'}</button
        >
      </div>
    </header>
    {#if !collapsed}
      <ul>
        {#each uploads.interrupted as saved (saved.upload_id)}
          <li class="interrupted">
            <div class="name" title={saved.uri}>{saved.name}</div>
            <div class="note">
              Interrupted by a reload. Choose {saved.name} again to resume; the parts already sent are
              kept.
            </div>
            <div class="row-actions">
              <label class="btn btn-small">
                Choose file…
                <input
                  type="file"
                  class="sr-only"
                  aria-label={`Choose ${saved.name} again to resume`}
                  onchange={(e) => reselect(saved, e)}
                />
              </label>
              <button
                class="link"
                onclick={() => {
                  // Its parts in S3 are deleted: there is no undo.
                  if (confirm(`Discard the upload of ${saved.name}? What was sent is deleted.`))
                    void uploads.discard(saved);
                }}>Discard<span class="sr-only"> {saved.name}</span></button
              >
            </div>
          </li>
        {/each}
        {#each uploads.elsewhere as saved (saved.upload_id)}
          <li class="interrupted">
            <div class="name" title={saved.uri}>{saved.name}</div>
            <div class="note">Uploading in another tab.</div>
          </li>
        {/each}
        {#each uploads.list as u (u.saved.upload_id)}
          <li class:failed={u.state === 'failed'}>
            <div class="name" title={u.uri}>{u.name}</div>
            <div class="meta">{fmtBytes(u.size)} · <span class="status">{status(u)}</span></div>
            {#if !u.finished}
              <progress max="100" value={percent(u)} aria-label={`${u.name} uploaded`}></progress>
              <div class="row-actions">
                {#if u.state === 'uploading'}
                  <button class="link" onclick={() => u.pause()}
                    >Pause<span class="sr-only"> {u.name}</span></button
                  >
                {:else if u.state === 'paused' || u.state === 'failed'}
                  <button class="link" onclick={() => u.run()}
                    >{u.state === 'failed' ? 'Retry' : 'Resume'}<span class="sr-only">
                      {u.name}</span
                    ></button
                  >
                {/if}
                {#if u.state !== 'completing'}
                  <button class="link danger" onclick={() => u.cancel()}
                    >Cancel<span class="sr-only"> {u.name}</span></button
                  >
                {/if}
              </div>
            {/if}
          </li>
        {/each}
      </ul>
      {#if reselectError}<p class="error" role="alert">{reselectError}</p>{/if}
    {/if}
  </section>
{/if}
<!-- Always present, so a change is announced even as the tray first appears. -->
<p class="sr-only" aria-live="polite">{announcement}</p>

<style>
  .tray {
    position: fixed;
    right: 1rem;
    bottom: 1rem;
    z-index: 40;
    width: min(24rem, calc(100vw - 2rem));
    max-height: 50vh;
    overflow: auto;
    border: 1px solid var(--control-border);
    border-radius: 0.55rem;
    background: var(--bg-content);
    color: var(--text);
    box-shadow: 0 6px 24px rgb(0 0 0 / 0.18);
    font-size: 0.82rem;
  }
  /* Above the launch form's sticky footer, so its Launch button stays reachable. */
  :global(body:has(footer.launch-bar)) .tray {
    bottom: 5.5rem;
  }
  /* Left of an open Learn panel, which sits on the right of a wide screen (on a narrow one the
     panel is a modal sheet, and the tray waits under it). */
  @media (min-width: 801px) {
    :global(body:has(#learn-panel)) .tray {
      right: calc(var(--learn-width) + 1rem);
      width: min(24rem, calc(100vw - var(--learn-width) - 2rem));
    }
  }
  header {
    position: sticky;
    top: 0;
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.5rem;
    padding: 0.5rem 0.75rem;
    background: var(--bg-content);
    border-bottom: 1px solid var(--border);
  }
  h2 {
    margin: 0;
    font-size: 0.88rem;
  }
  .count {
    font-weight: 400;
    color: var(--text-muted);
  }
  .head-actions {
    display: flex;
    align-items: center;
    gap: 0.5rem;
  }
  ul {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  li {
    padding: 0.55rem 0.75rem;
    border-bottom: 1px solid var(--border);
  }
  li:last-child {
    border-bottom: none;
  }
  .name {
    font-weight: 600;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .meta,
  .note {
    color: var(--text-muted);
    margin-top: 0.1rem;
  }
  .failed .status {
    color: var(--fg-fail);
  }
  progress {
    width: 100%;
    height: 0.4rem;
    margin-top: 0.35rem;
    accent-color: var(--accent);
  }
  .row-actions {
    display: flex;
    gap: 0.75rem;
    align-items: center;
    margin-top: 0.3rem;
  }
  .link.danger {
    color: var(--fg-fail);
  }
  .icon {
    border: none;
    background: none;
    color: var(--text-muted);
    cursor: pointer;
    font-size: 0.9rem;
  }
  .btn:focus-within,
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  .error {
    margin: 0;
    padding: 0.4rem 0.75rem 0.6rem;
  }
  /* Not underlined: the tray's row actions read as controls. */
  .link {
    text-decoration: none;
  }
</style>
