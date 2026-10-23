<script lang="ts">
  import { onMount } from 'svelte';
  import 'swagger-ui-dist/swagger-ui.css';
  import spec from '$lib/generated/openapi.json';
  import { errorMessage } from '$lib/util/format';

  let container = $state<HTMLDivElement | undefined>();
  let error = $state<string | null>(null);

  onMount(async () => {
    try {
      // swagger-ui-bundle touches window at module load, so it is imported here, in the browser,
      // and only when this page opens (it stays out of the main bundle). The spec is bundled at
      // build time from the backend's utoipa document — no runtime fetch, so it renders regardless
      // of API auth.
      const mod = await import('swagger-ui-dist/swagger-ui-bundle.js');
      const SwaggerUIBundle = mod.default ?? mod;
      SwaggerUIBundle({
        spec,
        domNode: container,
        // No deep links: Swagger UI's deep linking pushes history entries SvelteKit doesn't own, so
        // Back after leaving this page would change the URL without changing the page.
        deepLinking: false,
        // Read-only reference: "Try it out" against /api would need the app token + server rewrite;
        // keep it a browsable reference for now.
        supportedSubmitMethods: [],
      });
    } catch (e) {
      error = errorMessage(e);
    }
  });
</script>

<h1>API</h1>
<p class="muted">
  Reference for the biffix control-plane API (generated from the backend's OpenAPI document).
</p>

{#if error}
  <p class="error" role="alert">Couldn't render the API docs: {error}</p>
{/if}
<div class="swagger" bind:this={container}></div>

<style>
  /* Swagger UI has no dark theme of its own: its dark text needs a light background, so it sits on
     a white panel in either app theme. */
  .swagger {
    background: #fff;
    color-scheme: light;
    border-radius: 0.5rem;
    padding: 0 1rem;
  }
  /* Scope-neutralize a few Swagger UI defaults that fight the app chrome. */
  .swagger :global(.swagger-ui .topbar) {
    display: none;
  }
  .swagger :global(.swagger-ui .info) {
    margin: 1rem 0;
  }
  /* Swagger UI's method badges and version badges are white text on light colors (as low as
     1.6:1; swagger-ui #10702). Each is its own hue darkened to at least 4.5:1 (WCAG AA). */
  .swagger :global(.swagger-ui .opblock.opblock-get .opblock-summary-method) {
    background: #0172e3;
  }
  .swagger :global(.swagger-ui .opblock.opblock-post .opblock-summary-method) {
    background: #258358;
  }
  .swagger :global(.swagger-ui .opblock.opblock-put .opblock-summary-method) {
    background: #ad6103;
  }
  .swagger :global(.swagger-ui .opblock.opblock-delete .opblock-summary-method) {
    background: #e80707;
  }
  .swagger :global(.swagger-ui .opblock.opblock-patch .opblock-summary-method) {
    background: #15836a;
  }
  .swagger :global(.swagger-ui .info .title small) {
    background: #6e7583;
  }
  .swagger :global(.swagger-ui .info .title small.version-stamp) {
    background: #5a7e03;
  }
  /* Also light on white: the info block's links (the license's SPDX link, 3.3:1) and the
     JSON Schema 2020-12 view's "Expand all" buttons (1.9:1), which Swagger UI uses for an OpenAPI
     3.1 document. Darkened to at least 4.5:1. */
  .swagger :global(.swagger-ui .info a) {
    color: #1d68c4;
  }
  .swagger :global(.swagger-ui .json-schema-2020-12-expand-deep-button) {
    color: #5f6670;
  }
</style>
