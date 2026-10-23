<script lang="ts" generics="K extends string">
  // A sortable column header: a real button (focusable, Enter/Space) inside the `<th>`, which
  // carries `aria-sort` so the direction is announced and not only drawn.
  import { ariaSort, sortArrow, type SortDir } from '$lib/util/sort';

  let {
    label,
    key,
    sortKey,
    sortDir,
    onsort,
  }: {
    label: string;
    key: K;
    sortKey: K | null;
    sortDir: SortDir;
    onsort: (key: K) => void;
  } = $props();
</script>

<th scope="col" aria-sort={ariaSort(key, sortKey, sortDir)}>
  <button type="button" class="sort" onclick={() => onsort(key)}
    >{label}<span aria-hidden="true">{sortArrow(key, sortKey, sortDir)}</span></button
  >
</th>

<style>
  .sort {
    padding: 0;
    border: 0;
    background: none;
    font: inherit;
    letter-spacing: inherit;
    text-transform: inherit;
    color: inherit;
    text-align: inherit;
    cursor: pointer;
    user-select: none;
  }
  .sort:hover {
    color: var(--text-nav);
  }
  .sort:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
    border-radius: 0.2rem;
  }
</style>
