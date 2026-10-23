<script lang="ts">
  // Settings › View: Lab or Expert, saved for the user (/me/prefs), so it follows them to every
  // browser.
  import { setView, view } from '#lib/state/userPrefs.svelte.js';
</script>

<div class="settings-section">
  <h2>View <span class="scope">your account</span></h2>
  <p class="muted">Follows you to every browser you sign in from.</p>
  <div class="form">
    <fieldset class="field">
      <legend class="lbl">Show biffix as</legend>
      <label class="radio">
        <input
          type="radio"
          name="view"
          checked={view.mode === 'lab'}
          onchange={() => setView('lab')}
        />
        Lab <span class="hint">— analyses, samples and results, in plain words</span>
      </label>
      <label class="radio">
        <input
          type="radio"
          name="view"
          checked={view.mode === 'expert'}
          onchange={() => setView('expert')}
        />
        Expert <span class="hint">— jobs, logs, costs, engines and AWS details</span>
      </label>
    </fieldset>
    {#if view.saveError}
      <p class="error" role="alert">
        Couldn't save your view ({view.saveError}); it is kept in this browser and saved on your
        next visit.
      </p>
    {/if}
  </div>
</div>
