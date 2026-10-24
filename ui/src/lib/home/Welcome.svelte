<script lang="ts">
  // My work before the user's first analysis: the three steps to one, and the way to start.
  import { TOUR_ANCHOR } from '#lib/shell/tour.js';
</script>

<div class="welcome" data-tour={TOUR_ANCHOR.welcome}>
  <h2>You haven't started an analysis yet</h2>
  <ol>
    <li>
      <strong>Pick a pipeline</strong> for your experiment, such as RNA-seq or variant calling.
    </li>
    <li><strong>Add your samples</strong>: choose the folder your sequencing data is in.</li>
    <li>
      <strong>Launch</strong>, and follow each sample here. Results and QC appear when it finishes.
    </li>
  </ol>
  <a class="start" href="/runs/new">Start an analysis →</a>
</div>

<style>
  .start {
    padding: 0.55rem 1.1rem;
    border-radius: 0.5rem;
    background: var(--tab-active-bg);
    color: var(--tab-active-fg);
    text-decoration: none;
    font-weight: 600;
    white-space: nowrap;
  }
  .start:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
  h2 {
    font-size: 1rem;
    margin: 0 0 0.6rem;
  }
  .welcome {
    max-width: 640px;
    padding: 1.25rem 1.5rem;
    border: 1px solid var(--card-border);
    border-radius: 0.6rem;
  }
  .welcome h2 {
    margin-top: 0;
  }
  .welcome ol {
    padding-left: 1.2rem;
    line-height: 1.6;
    margin-bottom: 1.1rem;
  }
</style>
