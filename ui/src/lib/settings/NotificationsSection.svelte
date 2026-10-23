<script lang="ts">
  // Settings › Notifications (Lab view): what the bell tells you, and a way to mark it all read.
  import { markRead, notifications } from '$lib/notifications/notifications.svelte';
  import { unreadText } from './settings';

  // A refused read, said here where it was asked for (the count goes back up on its own).
  let refused = $state(false);
  async function markAll() {
    refused = !(await markRead({ all: true }));
  }
</script>

<div class="settings-section">
  <h2>Notifications <span class="scope">your account</span></h2>
  <p class="muted">
    The bell at the top right tells you when an analysis you started finishes or stops with a
    problem. Only your own analyses; email notifications aren't available yet.
  </p>
  <div class="actions">
    <span class="muted" data-testid="settings-unread"
      >{unreadText(notifications.list?.unread ?? null)}</span
    >
    {#if (notifications.list?.unread ?? 0) > 0}
      <button class="ghost" type="button" onclick={markAll}>Mark all as read</button>
    {/if}
  </div>
  {#if refused && notifications.readError}
    <p class="error" role="alert">Couldn't mark them read: {notifications.readError}</p>
  {/if}
</div>
