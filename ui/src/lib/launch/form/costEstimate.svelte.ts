// The launch form's cost estimate: what a run of this many samples usually costs, priced from the
// workflow's past runs, asked again (debounced) as the samples change. Where it stands is kept too,
// so the wizard can say it is still asking, or that asking failed, rather than "none".
//
// Construct it while a component initialises: its effect lives as long as that component.

import { getWorkflowEstimate } from '$lib/api';
import { delayed } from '$lib/util/debounce';
import { describeEstimate } from '../estimate';

/** What the estimate is asked for: read through getters, so they stay current. */
export interface EstimateInputs {
  /** The workflow version. */
  readonly id: string;
  /** The samples listed (none: no estimate is asked for). */
  readonly samples: number;
  /** A preview of an unregistered form has no runs to go by. */
  readonly preview: boolean;
}

export class CostEstimate {
  text = $state<string | null>(null);
  /** Whether an estimate is still being asked for. */
  estimating = $state(false);
  /** Whether asking for the estimate failed. */
  failed = $state(false);

  constructor(inputs: EstimateInputs) {
    $effect(() => {
      const n = inputs.samples;
      const id = inputs.id;
      this.text = null;
      this.estimating = false;
      this.failed = false;
      if (n === 0 || inputs.preview) return;
      this.estimating = true;
      let cancelled = false;
      const stop = delayed(() => {
        getWorkflowEstimate(id, n)
          .then((r) => {
            if (!cancelled) this.text = r.estimate ? describeEstimate(r.estimate, n) : null;
          })
          .catch(() => {
            // No estimate is fine; the wizard says it couldn't get one.
            if (!cancelled) this.failed = true;
          })
          .finally(() => {
            if (!cancelled) this.estimating = false;
          });
      }, 400);
      return () => {
        cancelled = true;
        stop();
      };
    });
  }
}
