<script lang="ts">
  // The alignment viewer: one BAM or CRAM output in igv.js, against the run's
  // reference bundle (its FASTA, or a genome igv hosts), opened where the reads are — or where the
  // person last was in this file against this reference. igv reads the files straight from storage
  // with HTTP Range requests through short-lived links, so the viewer checks it can before starting
  // igv (a bucket without a CORS rule for this site fails as a network error), and keeps the links
  // working while it is open (`LinkRenewal`): new ones before they expire and when igv's reads are
  // refused, igv kept where it is, and a failed renewal said beside it, never closing it. igv.js is
  // large: it is loaded only when a viewer opens (its own chunk).
  import { untrack } from 'svelte';
  import {
    ApiError,
    getAlignmentView,
    listReferences,
    type AlignmentLinks,
    type ReferenceBundle,
  } from '$lib/api';
  import Dialog from '$lib/components/Dialog.svelte';
  import { errorMessage } from '$lib/util/format';
  import {
    igvOptions,
    lastLocusKey,
    probeResultOf,
    probeSummary,
    probeUrls,
    referenceLabel,
    viewLinks,
    type IgvOptions,
    type ProbeResult,
  } from './igvConfig';
  import { LinkRenewal, noticeText, watchRefusedReads, type RenewalNotice } from './linkRenewal';

  /** The part of igv.js this viewer uses. */
  interface IgvBrowser {
    currentLoci(): string | string[];
    on(event: 'locuschange', handler: () => void): void;
    loadSessionObject(session: IgvOptions): Promise<void> | void;
  }
  interface IgvModule {
    default: {
      createBrowser(div: HTMLElement, options: IgvOptions): Promise<IgvBrowser>;
      removeBrowser(browser: IgvBrowser): void;
    };
  }

  interface Props {
    runId: string;
    outputId: string;
    /** What the viewer is titled (the result's name, and its sample). */
    title: string;
    onclose: () => void;
  }
  let { runId, outputId, title, onclose }: Props = $props();
  /** igv.js, imported lazily so it is its own chunk. */
  const loadIgv = () => import('igv') as unknown as Promise<IgvModule>;

  const uid = $props.id();
  const CORS_MESSAGE =
    "This storage isn't set up for the viewer yet; ask your admin to apply the CORS rule the deploy prints.";

  let view = $state<AlignmentLinks | null>(null);
  let error = $state<string | null>(null);
  let loading = $state(true);
  /** The bundle the person chose, for a run without one. */
  let chosenReference = $state<string | null>(null);
  let bundles = $state<ReferenceBundle[] | null>(null);
  let bundlesError = $state<string | null>(null);
  let pick = $state('');
  let probe = $state<ProbeResult | null>(null);
  let starting = $state(false);
  let container = $state<HTMLDivElement | null>(null);
  /** How the open browser's links stand (a failed renewal, refused reads); igv stays open. */
  let notice = $state<RenewalNotice>(null);
  /** The open browser's links (none before igv starts). */
  let renewal: LinkRenewal | null = null;

  const reference = $derived(view?.reference ?? null);

  function savedLocus(key: string): string | null {
    try {
      return sessionStorage.getItem(key);
    } catch {
      return null;
    }
  }
  function saveLocus(key: string, locus: string) {
    try {
      sessionStorage.setItem(key, locus);
    } catch {
      /* private mode: the viewer just opens at the reads next time */
    }
  }

  /** Ask for the links (and the reference); a 409 says why this file cannot be shown. */
  async function fetchView(ref: string | null, locus = true): Promise<AlignmentLinks> {
    try {
      return await getAlignmentView(runId, outputId, ref, { locus });
    } catch (e) {
      if (e instanceof ApiError && e.status === 409)
        throw new Error(`This file can't be shown in the genome browser: ${e.message}`);
      throw e;
    }
  }

  /** Can this browser read every file igv needs? One byte of each, with a Range request. */
  async function probeView(v: AlignmentLinks): Promise<ProbeResult> {
    const results = await Promise.all(
      probeUrls(v).map((url) =>
        fetch(url, { headers: { Range: 'bytes=0-0' } }).then(
          (r) => probeResultOf({ status: r.status }),
          (thrown) => probeResultOf({ thrown }),
        ),
      ),
    );
    return probeSummary(results);
  }

  // The links, asked for again whenever the output or the chosen reference changes.
  $effect(() => {
    void outputId;
    const ref = chosenReference;
    let cancelled = false;
    loading = true;
    error = null;
    view = null;
    probe = null;
    untrack(() => fetchView(ref))
      .then(async (v) => {
        if (cancelled) return;
        view = v;
        if (!v.reference) return;
        const p = await probeView(v);
        if (!cancelled) probe = p;
      })
      .catch((e) => {
        if (!cancelled) error = errorMessage(e);
      })
      .finally(() => {
        if (!cancelled) loading = false;
      });
    return () => {
      cancelled = true;
    };
  });

  // Without a reference: the assembly bundles that have a FASTA, to choose from.
  const needsReference = $derived(!!view && !reference);
  $effect(() => {
    if (!needsReference || bundles) return;
    listReferences({ kind: 'assembly', keys: ['fasta'] })
      .then((b) => (bundles = b))
      .catch((e) => (bundlesError = errorMessage(e)));
  });

  // igv itself, once the links are known to work, with its links kept working while it is open.
  const ready = $derived(!!view && !!reference && probe === 'ok' && !!container);
  // Started once per file and reference: a renewal replaces `view` without restarting igv.
  const startKey = $derived(ready ? `${outputId}\u0000${chosenReference ?? ''}` : null);
  $effect(() => {
    const div = container;
    if (startKey === null || !div) return;
    const first = untrack(() => view);
    if (!first) return;
    const key = lastLocusKey(runId, outputId, first.reference ?? null);
    const options = igvOptions(first, savedLocus(key));
    if (!options) return;
    const ref = untrack(() => chosenReference);
    let cancelled = false;
    let browser: IgvBrowser | null = null;
    let igv: IgvModule['default'] | null = null;
    let renewing: LinkRenewal | null = null;
    let unwatch: (() => void) | null = null;
    // The links igv is reading now: a refused read of an older one is not these links' fault.
    let links = new Set(viewLinks(first));
    const onVisible = () => {
      if (document.visibilityState === 'visible') renewing?.shown();
    };
    starting = true;
    notice = null;
    loadIgv()
      .then(async (mod) => {
        if (cancelled) return;
        igv = mod.default;
        const b = await igv.createBrowser(div, options);
        if (cancelled) {
          igv.removeBrowser(b);
          return;
        }
        browser = b;
        b.on('locuschange', () => {
          const locus = currentLocus(b);
          if (locus) saveLocus(key, locus);
        });
        renewing = new LinkRenewal(first.expires_at, {
          fetch: () => fetchView(ref, false),
          apply: async (fresh) => {
            // igv stays where it is; the server was not asked where the reads are.
            const at = currentLocus(b) || savedLocus(key) || untrack(() => view?.locus) || null;
            const next = igvOptions(fresh, at);
            if (!next) throw new Error('the new links have no reference');
            links = new Set(viewLinks(fresh));
            view = { ...fresh, locus: fresh.locus ?? untrack(() => view?.locus) };
            await b.loadSessionObject(next);
          },
          notice: (n) => (notice = n),
        });
        renewal = renewing;
        unwatch = watchRefusedReads(
          (url) => links.has(url),
          () => renewing?.refused(),
        );
        document.addEventListener('visibilitychange', onVisible);
      })
      .catch((e) => {
        if (!cancelled) error = `Couldn't start the genome browser: ${errorMessage(e)}`;
      })
      .finally(() => {
        if (!cancelled) starting = false;
      });
    return () => {
      cancelled = true;
      renewing?.stop();
      if (renewal === renewing) renewal = null;
      unwatch?.();
      document.removeEventListener('visibilitychange', onVisible);
      if (browser && igv) igv.removeBrowser(browser);
    };
  });

  /** Try again now (the notice's button). */
  function renewNow(): Promise<void> {
    return renewal?.renew() ?? Promise.resolve();
  }

  function currentLocus(b: IgvBrowser): string | null {
    const loci = b.currentLoci();
    return (Array.isArray(loci) ? loci[0] : loci) || null;
  }

  // An Escape igv handles itself (leaving its locus box, closing one of its popups) is not the
  // dialog's: without this, Escape in the locus box would close the whole viewer.
  const IGV_POPUPS = '.igv-ui-popover, .igv-menu-popup, .igv-ui-dropdown';
  function onIgvKeydown(e: KeyboardEvent) {
    if (e.key !== 'Escape' || !container) return;
    // igv draws in a shadow root on the container: the key's own target is inside it.
    const target = e.composedPath()[0];
    if (
      target instanceof HTMLElement &&
      (target.matches('input, textarea, select') || target.isContentEditable)
    ) {
      e.preventDefault();
      target.blur();
      return;
    }
    const root = container.shadowRoot ?? container;
    const open = [...root.querySelectorAll<HTMLElement>(IGV_POPUPS)].filter(
      (el) => el.getClientRects().length > 0,
    );
    if (open.length > 0) {
      e.preventDefault();
      for (const el of open) el.style.display = 'none';
    }
  }

  /** Links refused before igv started (expired, or denied): ask for new ones once, then say so. */
  // While new links are asked for the viewer says it is starting (being told to close it would
  // cancel the retry that fixes it). An answer for another file, or after closing, is dropped.
  let retried = false;
  let retryGen = 0;
  $effect(() => {
    void outputId;
    retried = false;
    return () => {
      retryGen++;
    };
  });
  $effect(() => {
    if (probe !== 'denied' || retried) return;
    retried = true;
    probe = null;
    const gen = retryGen;
    const ref = untrack(() => chosenReference);
    fetchView(ref)
      .then(async (v) => {
        const p = await probeView(v);
        if (gen !== retryGen) return;
        view = v;
        probe = p;
      })
      .catch((e) => {
        if (gen === retryGen) error = errorMessage(e);
      });
  });

  const corsUnknown = $derived(!!view && view.bucket_cors !== 'configured');
</script>

<Dialog labelledby="{uid}-title" {onclose} size="wide">
  <header>
    <div class="title">
      <h3 id="{uid}-title">{title}</h3>
      <p class="muted meta">
        {#if view}
          <span class="mono">{view.alignment.name}</span>
          {#if reference}{' · against '}{referenceLabel(reference)}{/if}
        {/if}
      </p>
    </div>
    <div class="actions">
      <button type="button" class="close" onclick={onclose} aria-label="Close">✕</button>
    </div>
  </header>
  <div class="body">
    {#if view?.notes?.length}
      <ul class="notes" aria-label="Notes">
        {#each view.notes as n}<li>{n}</li>{/each}
      </ul>
    {/if}
    {#if loading}
      <p class="muted">Loading…</p>
    {:else if error}
      <p class="error" role="alert">{error}</p>
    {:else if needsReference}
      <div class="choose">
        <p>This analysis has no reference genome to show the reads against. Choose one:</p>
        {#if bundlesError}
          <p class="error" role="alert">Couldn't load the references: {bundlesError}</p>
        {:else if bundles === null}
          <p class="muted">Loading the references…</p>
        {:else if bundles.length === 0}
          <p class="muted">There are no genome references yet; an admin adds them in Settings.</p>
        {:else}
          <form
            onsubmit={(e) => {
              e.preventDefault();
              if (pick) chosenReference = pick;
            }}
          >
            <label for="{uid}-ref">Reference</label>
            <select id="{uid}-ref" bind:value={pick}>
              <option value="" disabled>Choose a reference</option>
              {#each bundles as b (b.id)}<option value={b.id}>{b.display_name}</option>{/each}
            </select>
            <button type="submit" class="btn btn-primary" disabled={!pick}>Show</button>
          </form>
        {/if}
      </div>
    {:else if probe === 'cors'}
      <p class="error" role="alert">{CORS_MESSAGE}</p>
    {:else if probe === 'denied'}
      <p class="error" role="alert">
        The links to this file were refused: they may have expired, or the storage denied them.
        Close the viewer and open it again.
      </p>
    {:else if probe === 'error'}
      <p class="error" role="alert">The storage couldn't serve this file to the viewer.</p>
    {:else}
      {#if corsUnknown}
        <p class="muted small">
          If the reads don't load, this storage may not be set up for the viewer yet: ask your admin
          to apply the CORS rule the deploy prints.
        </p>
      {/if}
      {#if probe === null || starting}<p class="muted">Starting the genome browser…</p>{/if}
      <div class="renewal" role="status">
        {#if notice}
          <p class="error small">
            {noticeText(notice)}
            <button type="button" class="link" onclick={() => void renewNow()}>Try again</button>
          </p>
        {/if}
      </div>
      <!-- svelte-ignore a11y_no_static_element_interactions -->
      <div class="igv" data-testid="igv" bind:this={container} onkeydown={onIgvKeydown}></div>
    {/if}
  </div>
</Dialog>

<style>
  header {
    display: flex;
    align-items: flex-start;
    justify-content: space-between;
    gap: 1rem;
    padding: 0.75rem 1rem;
    border-bottom: 1px solid var(--border);
  }
  .title {
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: 0.95rem;
    overflow-wrap: anywhere;
  }
  .meta {
    margin: 0.2rem 0 0;
    font-size: 0.78rem;
    overflow-wrap: anywhere;
  }
  .body {
    padding: 0.75rem 1rem 1rem;
    overflow: auto;
    min-height: 12rem;
  }
  .igv {
    min-height: 24rem;
    /* igv draws its own light UI: a light frame, its controls light too, in either theme. */
    background: #fff;
    color: #000;
    color-scheme: light;
    border-radius: 0.35rem;
  }
  .notes {
    margin: 0 0 0.6rem;
    padding-left: 1.1rem;
    font-size: 0.8rem;
    color: var(--text-muted);
  }
  .choose form {
    display: flex;
    flex-wrap: wrap;
    align-items: center;
    gap: 0.5rem;
  }
  .choose label {
    font-weight: 600;
    font-size: 0.85rem;
  }
  select {
    min-width: 12rem;
    padding: 0.3rem 0.45rem;
    border: 1px solid var(--control-border);
    border-radius: 0.4rem;
    background: var(--bg-content);
    color: var(--text);
    font: inherit;
  }
  .small {
    font-size: 0.8rem;
  }
  .renewal p {
    margin: 0 0 0.5rem;
  }
  .close {
    background: none;
    border: none;
    cursor: pointer;
    font-size: 1rem;
    color: var(--text-muted);
    padding: 0 0.25rem;
  }
  .close:hover {
    color: inherit;
  }
  button:focus-visible,
  select:focus-visible {
    outline: 2px solid var(--accent);
    outline-offset: 2px;
  }
</style>
