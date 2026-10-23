import { h, now } from '../lib/clock.mjs';

// Experiments: named groups of a project's runs, as the backend keeps them (a table,
// not events; a run's membership is its `experiment_set` event, folded into `experiment_id`). Two
// with runs — Priya's liver study (batch-10 and batch-12) and an imported one (Bob's wgs run) —
// and an archived one with none. Their runs name them (`experiment_id`, in fixtures/runs.mjs).
export const EXPERIMENTS = [
  {
    id: 'exp_liver_tumor',
    project: 'genomics',
    name: 'Liver tumor study',
    description:
      'Tumor vs adjacent normal liver, mouse (C57BL/6). Bulk RNA-seq on two sequencing batches.',
    created_by: 'priya',
    created_by_label: 'priya@lab.example.org',
    created_at: now - 120 * h,
  },
  {
    id: 'exp_umi_tuning',
    project: 'imported',
    name: 'UMI consensus tuning',
    created_by: 'bob',
    created_by_label: 'bob@lab.example.org',
    created_at: now - 30 * h,
  },
  {
    id: 'exp_pilot_2025',
    project: 'genomics',
    name: 'Pilot 2025',
    description: 'The first look at the new library prep.',
    created_by: 'alice',
    created_by_label: 'alice@lab.example.org',
    created_at: now - 2000 * h,
    archived_at: now - 1500 * h,
  },
];
