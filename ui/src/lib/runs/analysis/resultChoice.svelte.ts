// One key result's chosen entry (a per-sample result picks its sample) and how that entry opens
// and downloads: the state `ResultCard` and the experiment table's `ResultCell` share.

import { outputViewerFile } from '#lib/components/viewerFile.js';
import { isServable, openKind, openTarget, sampleLabels } from '#lib/qc/open.js';
import type { ResultCard } from '#lib/runs/results.js';

export class ResultChoice {
  readonly #runId: () => string;
  readonly #card: () => ResultCard;
  /** The chosen entry, by its output (unique, where a sample need not be); the first by default. */
  chosen = $state<string | null>(null);
  /** Why the latest download failed, if it did. */
  downloadError = $state<string | null>(null);

  readonly entry = $derived.by(() => {
    const entries = this.#card().entries;
    return entries.find((e) => e.report.output.id === this.chosen) ?? entries[0] ?? null;
  });
  readonly output = $derived(this.entry?.report.output ?? null);
  /** The entry's own format: without an author's `kind` the server sets it per file. */
  readonly format = $derived(this.entry?.report.format ?? 'file');
  /** Each entry's picker label: a sample with several files (R1 and R2) names each file. */
  readonly labels = $derived.by(() =>
    sampleLabels(this.#card().entries.map((e) => ({ sample: e.sample, output: e.report.output }))),
  );
  readonly file = $derived.by(() => {
    const card = this.#card();
    const output = this.output;
    if (!output) return null;
    return outputViewerFile(this.#runId(), output, {
      name: this.entry?.sample ? `${card.name} · ${this.entry.sample}` : card.name,
      detail: card.description,
      onDownloadError: (m) => (this.downloadError = m),
    });
  });
  /** A file that is gone, or that biffix issues no link for, is neither opened nor downloaded. */
  readonly served = $derived(this.output ? isServable(this.output) : false);
  /** What Open shows, when the entry opens in a viewer; `null` when it only downloads. */
  readonly target = $derived.by(() => {
    const { output, file } = this;
    const kind = output && this.served ? openKind(this.format, output) : null;
    return kind && output && file ? openTarget(kind, this.#runId(), output, file) : null;
  });

  constructor(runId: () => string, card: () => ResultCard) {
    this.#runId = runId;
    this.#card = card;
  }
}
