<script lang="ts">
  // Shown in Lab view on a page that only exists in the Expert view (a colleague's link to a run's
  // Jobs tab, say): the page still renders, and this offers the way back without changing the
  // user's preference.
  import { page } from '$app/state';
  import { labEquivalent } from '#lib/view/viewRoutes.js';
  import { t } from '#lib/view/vocab.js';

  const back = $derived(labEquivalent(page.url.pathname));
  // The words are the Lab's: an analysis is a run, a pipeline a workflow.
  const backLabel = $derived(
    back === '/'
      ? `Back to ${t('home', 'lab')}`
      : back.startsWith('/workflows/')
        ? 'Back to the Lab view of this pipeline'
        : 'Back to the Lab view of this analysis',
  );
</script>

<div class="banner" role="note">
  You're looking at an Expert page. <a href={back}>{backLabel}</a>
</div>

<style>
  .banner {
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  a {
    color: var(--accent);
    text-decoration: underline;
  }
</style>
