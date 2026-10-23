<script lang="ts">
  // Settings → References: the stored bundles, each with its files' keys, Edit and Delete (which
  // asks once more first).
  import type { ReferenceBundle } from '#lib/api/index.js';
  import { fmtRelativeMicros } from '#lib/util/format.js';
  import { kindLabel } from './bundleForm';

  interface Props {
    bundles: ReferenceBundle[];
    /** The bundle whose Delete has been pressed once (it now says Confirm delete). */
    confirmDelete: string | null;
    onedit: (b: ReferenceBundle) => void;
    ondelete: (id: string) => void;
  }
  let { bundles, confirmDelete, onedit, ondelete }: Props = $props();
</script>

<table>
  <thead>
    <tr>
      <th scope="col">Reference</th>
      <th scope="col">Kind</th>
      <th scope="col">Files</th>
      <th scope="col">Updated</th>
      <th scope="col"><span class="sr-only">Actions</span></th>
    </tr>
  </thead>
  <tbody>
    {#each bundles as b (b.id)}
      <tr>
        <td>
          <div class="name">{b.display_name}</div>
          <code class="id">{b.id}</code>
          {#if b.igv_genome}<div class="notes">
              Alignments shown on hosted {b.igv_genome}
            </div>{/if}
          {#if b.notes}<div class="notes">{b.notes}</div>{/if}
        </td>
        <td>{kindLabel(b.kind)}</td>
        <td>
          <div class="keys">
            {#each Object.entries(b.uris) as [key, uri]}<span class="key-chip" title={uri}
                >{key}</span
              >{/each}
          </div>
        </td>
        <td class="muted">{fmtRelativeMicros(b.updated_at_micros)}</td>
        <td class="row-actions">
          <button class="link" onclick={() => onedit(b)}
            >Edit<span class="sr-only"> {b.id}</span></button
          >
          <button class="link danger" onclick={() => ondelete(b.id)}
            >{confirmDelete === b.id ? 'Confirm delete' : 'Delete'}<span class="sr-only">
              {b.id}</span
            ></button
          >
        </td>
      </tr>
    {/each}
  </tbody>
</table>

<style>
  button:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
  /* Not underlined: Edit and Delete are row actions, not links in a sentence. */
  .link {
    text-decoration: none;
    font-size: 0.85rem;
  }
  .link.danger {
    color: var(--fg-fail);
  }
  table {
    border-collapse: collapse;
    width: 100%;
    max-width: 64rem;
    font-size: 0.85rem;
  }
  th,
  td {
    text-align: left;
    padding: 0.55rem 0.6rem;
    border-bottom: 1px solid var(--border);
    vertical-align: top;
  }
  th {
    font-size: 0.75rem;
    color: var(--text-muted);
    font-weight: 600;
  }
  .name {
    font-weight: 600;
  }
  .id {
    font-family: var(--mono);
    font-size: 0.75rem;
    color: var(--text-muted);
  }
  code {
    font-family: var(--mono);
    font-size: 0.85em;
  }
  .notes {
    font-size: 0.78rem;
    color: var(--text-muted);
    margin-top: 0.15rem;
  }
  .keys {
    display: flex;
    gap: 0.3rem;
    flex-wrap: wrap;
  }
  .key-chip {
    font-family: var(--mono);
    font-size: 0.72rem;
    padding: 0.05rem 0.4rem;
    border-radius: 0.6rem;
    border: 1px solid var(--border);
  }
  .row-actions {
    white-space: nowrap;
    text-align: right;
  }
  .row-actions .link + .link {
    margin-left: 0.75rem;
  }
  @media (max-width: 640px) {
    table,
    thead,
    tbody,
    tr,
    td {
      display: block;
    }
    thead {
      display: none;
    }
    td {
      border-bottom: none;
      padding: 0.2rem 0;
    }
    tr {
      border-bottom: 1px solid var(--border);
      padding: 0.5rem 0;
    }
    .row-actions {
      text-align: left;
    }
  }
</style>
