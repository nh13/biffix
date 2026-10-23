<script lang="ts">
  // Settings › Alerts: who is emailed when an operational alarm fires or recovers (the alerts
  // topic's email subscribers; developer only).
  import { recipientStatus } from './settings';
  import type { AlertsState } from './settingsState.svelte';

  let { alerts }: { alerts: AlertsState } = $props();

  let newRecipient = $state('');
  async function addRecipient() {
    const email = newRecipient.trim();
    if (!email) return;
    if (await alerts.add(email)) newRecipient = '';
  }
</script>

<div class="settings-section">
  <h2>Alerts</h2>
  <p class="muted">
    Who is emailed when an operational alarm fires or recovers (the dead-letter queue fills, ingest
    stalls, the reconciler stops, the API errors). A new address must confirm from the email AWS
    sends it.
  </p>
  <div class="form">
    {#if alerts.alerts && !alerts.alerts.configured}
      <p class="muted">This deployment has no alerts topic.</p>
    {:else if alerts.alerts}
      {#if alerts.alerts.recipients.length === 0}
        <p class="muted">No recipients yet.</p>
      {:else}
        <ul class="recipients">
          {#each alerts.alerts.recipients as r (r.email)}
            <li>
              <span class="email">{r.email}</span>
              <span class={`status ${r.status}`}>{recipientStatus(r.status)}</span>
              {#if r.status === 'confirmed'}
                <button
                  class="ghost"
                  type="button"
                  disabled={alerts.busy}
                  onclick={() => alerts.remove(r.email)}>Remove</button
                >
              {:else if r.status === 'pending'}
                <span class="hint">lapses after 3 days unconfirmed</span>
              {/if}
            </li>
          {/each}
        </ul>
      {/if}
      <div class="add-row">
        <input
          type="email"
          aria-label="Recipient email"
          placeholder="name@example.org"
          bind:value={newRecipient}
          onkeydown={(e) => e.key === 'Enter' && !alerts.busy && addRecipient()}
        />
        <button
          class="ghost"
          type="button"
          disabled={alerts.busy || !newRecipient.trim()}
          onclick={addRecipient}>Add</button
        >
      </div>
    {:else if !alerts.error}
      <p class="muted">Loading…</p>
    {/if}
    {#if alerts.error}<span class="error" role="alert">{alerts.error}</span>{/if}
  </div>
</div>

<style>
  .recipients {
    list-style: none;
    margin: 0;
    padding: 0;
    width: 100%;
    display: flex;
    flex-direction: column;
    gap: 0.45rem;
  }
  .recipients li {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    font-size: 0.85rem;
  }
  .email {
    font-family: var(--mono);
  }
  .status {
    font-size: 0.7rem;
    padding: 0.05rem 0.4rem;
    border-radius: 0.3rem;
    border: 1px solid var(--border);
    color: var(--text-muted);
  }
  .status.confirmed {
    color: var(--fg-ok);
    border-color: #30a46c55;
  }
  .status.pending {
    color: var(--fg-warn);
    border-color: #f5a62355;
  }
  .add-row {
    display: flex;
    gap: 0.5rem;
    width: 100%;
  }
  .add-row input {
    flex: 1;
  }
</style>
