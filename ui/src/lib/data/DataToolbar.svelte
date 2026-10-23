<script lang="ts">
  // The Data browser's tools over the folder: the "starts with" filter, the search under the folder
  // and, where uploads go (the Lab view's), the upload button.
  import type { FolderListing } from './folderListing.svelte';
  import type { DataPresentation } from './presentation';

  interface Props {
    presentation: DataPresentation;
    listing: FolderListing;
    /** Whether files can be uploaded here (the upload button shows). */
    canUpload: boolean;
    /** Whether the upload form is open. */
    uploadOpen: boolean;
    onupload: () => void;
  }
  let { presentation: p, listing, canUpload, uploadOpen, onupload }: Props = $props();
</script>

<div class="tools">
  <input
    type="search"
    placeholder="Name starts with…"
    aria-label="Filter this folder by name"
    bind:value={listing.startsWith}
    oninput={() => listing.filterChanged()}
    onkeydown={(e) => e.key === 'Enter' && listing.search()}
  />
  <button
    class="btn btn-compact"
    onclick={() => listing.search()}
    disabled={!listing.startsWith.trim()}>{p.searchButton}</button
  >
  {#if canUpload && p.uploads}
    <button class="btn btn-compact btn-primary" onclick={onupload} aria-expanded={uploadOpen}
      >{p.uploads.button}</button
    >
  {/if}
</div>

<style>
  .tools {
    display: flex;
    gap: 0.5rem;
    flex-wrap: wrap;
    margin-bottom: 0.6rem;
  }
  .tools input {
    font: inherit;
    font-size: 0.85rem;
    padding: 0.35rem 0.55rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    width: min(100%, 18rem);
  }
  button:focus-visible,
  input:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 1px;
  }
</style>
