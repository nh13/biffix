<script lang="ts">
  // Settings › Display: per-browser preferences (localStorage), applied immediately, no Save and no
  // access needed — unlike the view (saved for the user) and the site settings (what every viewer
  // sees).
  import {
    appearance,
    prefs,
    setLogOrder,
    setThemeChoice,
    type LogOrder,
    type ThemeChoice,
  } from '#lib/state/localPrefs.svelte.js';
  import { view } from '#lib/state/userPrefs.svelte.js';

  const THEMES: { id: ThemeChoice; label: string; hint: string }[] = [
    { id: 'system', label: 'Match my computer', hint: 'light or dark, as your computer is set' },
    { id: 'light', label: 'Light', hint: 'dark text on white' },
    { id: 'dark', label: 'Dark', hint: 'light text on a dark background' },
  ];
  const LOG_ORDERS: { id: LogOrder; label: string; hint: string }[] = [
    { id: 'oldest', label: 'Oldest first', hint: 'reads top-down, like a terminal' },
    { id: 'newest', label: 'Newest first', hint: 'latest line at the top' },
  ];
</script>

<div class="settings-section">
  <h2>Display <span class="scope">this browser</span></h2>
  <p class="muted">Personal preferences, saved in this browser only.</p>
  <div class="form">
    <fieldset class="field">
      <legend class="lbl">Theme</legend>
      {#each THEMES as t (t.id)}
        <label class="radio">
          <input
            type="radio"
            name="theme"
            checked={appearance.theme === t.id}
            onchange={() => setThemeChoice(t.id)}
          />
          {t.label} <span class="hint">— {t.hint}</span>
        </label>
      {/each}
    </fieldset>
    <!-- Logs are an Expert concern: the Lab view shows none beyond a failed step's last lines. -->
    {#if view.mode === 'expert'}
      <fieldset class="field">
        <legend class="lbl">Log order</legend>
        {#each LOG_ORDERS as o (o.id)}
          <label class="radio">
            <input
              type="radio"
              name="log-order"
              checked={prefs.logOrder === o.id}
              onchange={() => setLogOrder(o.id)}
            />
            {o.label} <span class="hint">— {o.hint}</span>
          </label>
        {/each}
      </fieldset>
    {/if}
  </div>
</div>
