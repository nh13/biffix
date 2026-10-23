// Samples detected from picked files (pick → detect → preview → add): the files, the naming rule,
// the server's grouping of them, and the naming editor's state. A naming change re-detects after a
// pause; closing the preview drops any detection still pending or in flight.

import { detectSamples, type DetectedSamples } from '$lib/api';
import type { DetectRule, SheetSpec } from '$lib/generated/samples';
import { debounce } from '$lib/util/debounce';
import { errorMessage } from '$lib/util/format';
import { autoRule, detectionSummary, exampleSampleId, selectedTokens, toggleToken } from './detect';

export class SampleDetection {
  readonly #spec: () => SheetSpec;
  files = $state<string[]>([]);
  rule = $state<DetectRule>(autoRule());
  preview = $state<DetectedSamples | null>(null);
  detecting = $state(false);
  error = $state<string | null>(null);
  /** Whether the naming editor is open. */
  naming = $state(false);
  regex = $state('');

  readonly selectedTokens = $derived(selectedTokens(this.rule));
  readonly exampleId = $derived(this.preview ? exampleSampleId(this.preview, this.files[0]) : null);
  readonly summary = $derived(this.preview ? detectionSummary(this.preview) : '');

  #request = 0;
  // A naming change re-detects after a pause, so each click or keystroke does not.
  readonly #soon = debounce(() => void this.run(), 250);

  /** `spec`: the samplesheet the detected samples fill (the response carries its rows). */
  constructor(spec: () => SheetSpec) {
    this.#spec = spec;
  }

  async run() {
    const id = ++this.#request;
    this.detecting = true;
    this.error = null;
    try {
      const response = await detectSamples({
        files: this.files.map((uri) => ({ uri })),
        rule: this.rule,
        samplesheet: this.#spec(),
      });
      if (id === this.#request) this.preview = response;
    } catch (e) {
      if (id === this.#request) this.error = errorMessage(e);
    } finally {
      if (id === this.#request) this.detecting = false;
    }
  }

  // Close the preview and drop any detection still pending or in flight, so a late response
  // cannot bring it back.
  close() {
    this.#soon.cancel();
    this.#request++;
    this.detecting = false;
    this.error = null;
    this.preview = null;
    this.files = [];
    this.naming = false;
  }

  /** Detect the samples in files just picked, with the default rule. */
  pick(uris: string[]) {
    this.close();
    this.files = uris;
    this.rule = autoRule();
    void this.run();
  }

  setRule(next: DetectRule) {
    this.rule = next;
    this.#soon();
  }

  toggleToken(i: number) {
    this.setRule({
      ...this.rule,
      method: { kind: 'tokens', selected: toggleToken(this.selectedTokens, i) },
    });
  }
}
