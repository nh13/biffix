<script lang="ts">
  // Settings: one section per heading (lib/settings), sharing the form styles below. The page loads
  // what the sections show once, in either view: the site settings (the Expert view's Site section,
  // the Lab view's Who to ask), the engine releases and the alert recipients.
  import PinnedPipelines from '#lib/catalog/PinnedPipelines.svelte';
  import { view } from '#lib/state/userPrefs.svelte.js';
  import {
    AlertsState,
    EnginesState,
    SiteSettingsState,
  } from '#lib/settings/settingsState.svelte.js';
  import ViewSection from '#lib/settings/ViewSection.svelte';
  import SiteSection from '#lib/settings/SiteSection.svelte';
  import EnginesSection from '#lib/settings/EnginesSection.svelte';
  import AlertsSection from '#lib/settings/AlertsSection.svelte';
  import DisplaySection from '#lib/settings/DisplaySection.svelte';
  import NotificationsSection from '#lib/settings/NotificationsSection.svelte';
  import WhoToAskSection from '#lib/settings/WhoToAskSection.svelte';
  import HelpSection from '#lib/settings/HelpSection.svelte';

  const settings = new SiteSettingsState();
  const engines = new EnginesState();
  const alerts = new AlertsState();
  $effect(() => {
    settings.load();
  });
  $effect(() => engines.load());
  $effect(() => alerts.load());
</script>

<h1>Settings</h1>

<ViewSection />

{#if view.mode === 'lab'}
  <p class="muted">Site, reference, engine and alert settings are in the Expert view.</p>
{:else}
  <SiteSection {settings} />

  <div class="settings-section">
    <h2>References</h2>
    <p class="muted">
      Reference genomes and assay kits that launch forms offer in their reference dropdown.
      <a href="/settings/references">Manage references →</a>
    </p>
  </div>

  <EnginesSection {engines} />
  <AlertsSection {alerts} />
{/if}

<DisplaySection />

{#if view.mode === 'lab'}
  <NotificationsSection />

  <div class="settings-section">
    <h2>Pinned pipelines <span class="scope">your account</span></h2>
    <p class="muted">
      Pinned pipelines sit on My work for a quick start. Pin one with its star on the
      <a href="/workflows">Pipelines</a> page.
    </p>
  </div>
  <!-- Outside a section: the list keeps its own styles. -->
  <PinnedPipelines />

  <WhoToAskSection {settings} />
{/if}

<HelpSection />

<style>
  /* The sections (lib/settings) are each a `.settings-section`, and share these form styles. */
  :global(.settings-section) :global {
    /* A link inside running text is underlined, not told apart by colour alone. */
    p a {
      text-decoration: underline;
    }
    .form {
      display: flex;
      flex-direction: column;
      align-items: flex-start;
      gap: 1.1rem;
      max-width: 520px;
    }
    label,
    .field {
      width: 100%;
    }
    .field,
    label {
      display: flex;
      flex-direction: column;
      gap: 0.35rem;
    }
    .lbl {
      font-size: 0.78rem;
      font-weight: 600;
      color: var(--text-muted);
    }
    input[type='text'],
    input[type='email'],
    input[type='url'] {
      padding: 0.45rem 0.6rem;
      border: 1px solid var(--border);
      border-radius: 0.4rem;
      background: var(--bg-content);
      color: var(--text);
    }
    /* Inline after a label or a heading, so a step smaller than the shared hint. */
    .hint {
      font-size: 0.72rem;
    }
    /* Wider apart than the shared row: a button and the status text after it. */
    .actions {
      gap: 0.8rem;
    }
    .actions button {
      padding: 0.4rem 1rem;
      border: 1px solid var(--border);
      border-radius: 0.4rem;
      background: var(--accent, #4a7cf5);
      color: #fff;
      cursor: pointer;
    }
    .actions button.ghost {
      background: var(--bg-content);
      color: var(--text);
    }
    h3 {
      margin: 1.4rem 0 0.3rem;
      font-size: 0.92rem;
    }
    .actions button:disabled {
      opacity: 0.5;
      cursor: default;
    }
    .ghost {
      padding: 0.25rem 0.6rem;
      border: 1px solid var(--border);
      border-radius: 0.4rem;
      background: var(--bg-content);
      color: var(--text);
      cursor: pointer;
    }
    .ok {
      color: var(--fg-ok);
      font-size: 0.85rem;
    }
    h2 {
      font-size: 1rem;
      margin: 1.6rem 0 0.2rem;
    }
    .scope {
      font-size: 0.72rem;
      font-weight: 500;
      color: var(--text-muted);
      margin-left: 0.4rem;
    }
    fieldset {
      border: none;
      margin: 0;
      padding: 0;
    }
    label.radio {
      flex-direction: row;
      align-items: center;
      gap: 0.45rem;
      font-size: 0.85rem;
    }
  }
</style>
