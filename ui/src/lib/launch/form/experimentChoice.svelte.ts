// The experiments a launch can go into: the version's project's open ones, read once the wizard (or
// an experiment already chosen) needs them. A chosen experiment the project does not offer open
// (archived, another project's) is dropped, and remembered so the form can say so.
//
// Construct it while a component initialises: its effects live as long as that component.

import { listExperiments, type Experiment } from '#lib/api/index.js';
import { errorMessage } from '#lib/util/format.js';

/** What the list is read for: read through getters, so they stay current. */
export interface ExperimentInputs {
  /** The version's project (none: no experiments to offer). */
  readonly project: string | null;
  /** Whether the list is needed at all. */
  readonly needed: boolean;
  /** The experiment chosen, which the form owns (its draft and undo keep it). */
  experiment: string | null;
}

export class ExperimentChoice {
  list = $state<Experiment[] | null>(null);
  error = $state<string | null>(null);
  /** A preselected experiment that is not open in this project (archived, another project's). */
  unavailable = $state<string | null>(null);
  /** Whether the new-experiment form is open in place of the dropdown. */
  creating = $state(false);

  constructor(inputs: ExperimentInputs) {
    $effect(() => {
      const p = inputs.project;
      if (!inputs.needed || !p) return;
      let cancelled = false;
      listExperiments({ project: p })
        .then((l) => {
          if (cancelled) return;
          this.list = l.experiments;
          this.error = null;
        })
        .catch((e) => {
          if (!cancelled) this.error = errorMessage(e);
        });
      return () => {
        cancelled = true;
      };
    });
    // A chosen experiment the project does not offer is dropped, and the Review says so.
    $effect(() => {
      const list = this.list;
      const chosen = inputs.experiment;
      if (!list || !chosen) return;
      if (!list.some((e) => e.id === chosen && e.archived_at == null)) {
        this.unavailable = chosen;
        inputs.experiment = null;
      }
    });
  }
}
